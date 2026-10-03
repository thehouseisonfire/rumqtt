#![allow(non_camel_case_types)]
// The exported operations share one safety contract: every non-null pointer must satisfy the
// ownership, lifetime, alignment, readability, and writability requirements documented in
// `rumqttc.h`. Repeating that contract on every C entry point would obscure the ABI surface.
#![allow(clippy::missing_safety_doc)]
// C callers cannot express Rust's `unsafe` qualifier. Every exported entry
// point validates nullable arguments and confines dereferences to explicit
// unsafe blocks inside the panic boundary.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

#[path = "tls.rs"]
mod tls;
pub use tls::*;

#[path = "websocket.rs"]
mod websocket;
pub use websocket::*;

#[path = "transport.rs"]
mod transport;
pub use transport::*;

use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::slice;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use rumqttc_wrapper_core::{
    AcknowledgementProtocolOptions, Admission, AsyncAuthChallenge, AsyncAuthenticator,
    AsyncAuthenticatorConfig, AuthAction, AuthContext, AuthFailure, AuthFuture, AuthProperties,
    Command, Completion, DiagnosticsSnapshot, DisconnectProtocolOptions, IncomingPublish,
    LastWillConfig, LastWillProtocolOptions, OutgoingActivity, ProtocolVersion, ProxyConfig,
    ProxyCredentials, PublishCommand, PublishCompletion, PublishProtocolOptions, QoS, SecretBytes,
    SessionCheckpoint, SessionStore, SessionStoreConfig, SessionStoreKey, SrvFailure, SrvFuture,
    SrvRecord, SrvResolver, SrvResolverConfig, StoreFailure, StoreFuture, SubscribeCommand,
    SubscribeProtocolOptions, SubscribeResult, Subscription, SubscriptionProtocolOptions,
    TlsBackend, TlsClientIdentity, TlsConfig, TlsRootPolicy, TopicAliasPolicy, UnsubscribeCommand,
    UnsubscribeProtocolOptions, UnsubscribeResult, V5AcknowledgementOptions, V5ConnectProperties,
    V5DisconnectOptions, V5IncomingPublishProperties, V5OutgoingPublishProperties,
    V5RetainForwardRule, V5SubscribeProperties, V5SubscriptionOptions, V5UnsubscribeProperties,
    V5WillProperties, WebSocketHeader, WrapperEvent,
};

use crate::client::{ClientError, ClientObject};
use crate::completion::CompletionObject;
use crate::config::{
    ConfigHandle, set_ack_mode, set_connection_timeout, set_event_delivery_timeout, set_keep_alive,
    set_transport_tcp, set_transport_tls, set_transport_websocket, set_transport_wss,
    set_v4_clean_session, set_v5_session, tls_config,
};
use crate::error::{ErrorHandle, OK, TIMEOUT, WOULD_BLOCK};
use crate::event::EventObject;

const ABI_VERSION: u32 = 1;
const MQTT5_NO_SUBSCRIPTION_EXISTED: u8 = 0x11;
const PROTOCOL_OPTIONS_VERSION_NEUTRAL: u32 = 0;
const PROTOCOL_OPTIONS_V5: u32 = 5;

const CAP_V4: u64 = 1 << 0;
const CAP_V5: u64 = 1 << 1;
const CAP_RUSTLS: u64 = 1 << 2;
const CAP_NATIVE_TLS: u64 = 1 << 3;
const CAP_WEBSOCKET: u64 = 1 << 4;
const CAP_HTTP_PROXY: u64 = 1 << 5;
const CAP_SOCKS5_PROXY: u64 = 1 << 6;
const CAP_UNIX: u64 = 1 << 7;
const CAP_SYSTEM_SRV: u64 = 1 << 8;
const CAP_SCRAM: u64 = 1 << 9;
const CAP_TRACING: u64 = 1 << 10;
const CAP_STORE_CALLBACKS: u64 = 1 << 11;
const CAP_AUTH_CALLBACKS: u64 = 1 << 12;
const CAP_TRANSPORT_CALLBACKS: u64 = 1 << 13;
const CAP_WEBSOCKET_CALLBACKS: u64 = 1 << 14;
const MAX_CHECKPOINT_SIZE: usize = 256 * 1024 * 1024;

#[repr(C)]
pub struct rumqttc_v5_will_properties_t {
    pub struct_size: u32,
    pub will_delay_present: u8,
    pub payload_format_present: u8,
    pub message_expiry_present: u8,
    pub content_type_present: u8,
    pub response_topic_present: u8,
    pub correlation_data_present: u8,
    pub reserved: [u8; 2],
    pub will_delay_interval: u32,
    pub payload_format_indicator: u32,
    pub message_expiry_interval: u32,
    pub content_type: rumqttc_string_view_t,
    pub response_topic: rumqttc_string_view_t,
    pub correlation_data: rumqttc_bytes_view_t,
    pub user_properties: *const rumqttc_user_property_t,
    pub user_property_count: usize,
}

#[repr(C)]
pub struct rumqttc_last_will_t {
    pub struct_size: u32,
    pub topic: rumqttc_string_view_t,
    pub payload: rumqttc_bytes_view_t,
    pub qos: u32,
    pub retain: u8,
    pub reserved: [u8; 3],
    pub protocol_options: u32,
    pub v5_properties: *const rumqttc_v5_will_properties_t,
}

#[repr(C)]
pub struct rumqttc_v5_connect_properties_t {
    pub struct_size: u32,
    pub session_expiry_present: u8,
    pub receive_maximum_present: u8,
    pub maximum_packet_size_present: u8,
    pub topic_alias_maximum_present: u8,
    pub request_response_info_present: u8,
    pub request_problem_info_present: u8,
    pub authentication_method_present: u8,
    pub authentication_data_present: u8,
    pub reserved: [u8; 4],
    pub session_expiry_interval: u32,
    pub receive_maximum: u32,
    pub maximum_packet_size: u32,
    pub topic_alias_maximum: u32,
    pub request_response_information: u8,
    pub request_problem_information: u8,
    pub reserved_tail: [u8; 2],
    pub authentication_method: rumqttc_string_view_t,
    pub authentication_data: rumqttc_bytes_view_t,
    pub user_properties: *const rumqttc_user_property_t,
    pub user_property_count: usize,
}

#[repr(C)]
pub struct rumqttc_websocket_header_edit_t {
    pub struct_size: u32,
    pub operation: u32,
    pub name: rumqttc_string_view_t,
    pub value: rumqttc_string_view_t,
    pub reserved: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_v5_disconnect_properties_t {
    pub struct_size: u32,
    pub reason_code: u32,
    pub session_expiry_present: u8,
    pub reason_string_present: u8,
    pub server_reference_present: u8,
    pub reserved: [u8; 5],
    pub session_expiry_interval: u32,
    pub reason_string: rumqttc_string_view_t,
    pub server_reference: rumqttc_string_view_t,
    pub user_properties: *const rumqttc_user_property_t,
    pub user_property_count: usize,
}

#[repr(C)]
pub struct rumqttc_disconnect_options_t {
    pub struct_size: u32,
    pub protocol_options: u32,
    pub v5_properties: *const rumqttc_v5_disconnect_properties_t,
    pub reserved: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_v5_acknowledgement_options_t {
    pub struct_size: u32,
    pub reason_code: u32,
    pub reason_string_present: u8,
    pub reserved: [u8; 7],
    pub reason_string: rumqttc_string_view_t,
    pub user_properties: *const rumqttc_user_property_t,
    pub user_property_count: usize,
}

#[repr(C)]
pub struct rumqttc_acknowledgement_options_t {
    pub struct_size: u32,
    pub protocol_options: u32,
    pub v5_options: *const rumqttc_v5_acknowledgement_options_t,
    pub reserved: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_tls_pem_identity_t {
    pub struct_size: u32,
    pub reserved: u32,
    pub certificate: rumqttc_bytes_view_t,
    pub private_key: rumqttc_bytes_view_t,
    pub reserved_tail: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_tls_pkcs12_identity_t {
    pub struct_size: u32,
    pub reserved: u32,
    pub identity: rumqttc_bytes_view_t,
    pub password: rumqttc_bytes_view_t,
    pub reserved_tail: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_tls_options_t {
    pub struct_size: u32,
    pub backend: u32,
    pub root_policy: u32,
    pub reserved: u32,
    pub ca_pem: rumqttc_bytes_view_t,
    pub pem_identity: *const rumqttc_tls_pem_identity_t,
    pub pkcs12_identity: *const rumqttc_tls_pkcs12_identity_t,
    pub alpn_protocols: *const rumqttc_bytes_view_t,
    pub alpn_protocol_count: usize,
    pub reserved_tail: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_proxy_options_t {
    pub struct_size: u32,
    pub protocol: u32,
    pub dns_policy: u32,
    pub reserved: u32,
    pub host: rumqttc_string_view_t,
    pub port: u32,
    pub username: rumqttc_bytes_view_t,
    pub password: rumqttc_bytes_view_t,
    pub credentials_present: u8,
    pub reserved_tail: [u8; 7],
    pub tls: *const rumqttc_tls_options_t,
}

#[repr(C)]
pub struct rumqttc_store_request_t {
    pub struct_size: u32,
    pub operation: u32,
    pub protocol: u32,
    pub checkpoint_format_version: u32,
    pub scope: rumqttc_string_view_t,
    pub client_id: rumqttc_string_view_t,
    pub checkpoint: rumqttc_bytes_view_t,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_store_vtable_t {
    pub struct_size: u32,
    pub load: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_store_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub save: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_store_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub clear: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_store_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_resolver_request_t {
    pub struct_size: u32,
    pub owner: rumqttc_string_view_t,
}

#[repr(C)]
pub struct rumqttc_srv_record_t {
    pub struct_size: u32,
    pub priority: u32,
    pub weight: u32,
    pub port: u32,
    pub reserved: u32,
    pub target: rumqttc_string_view_t,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_resolver_vtable_t {
    pub struct_size: u32,
    pub resolve: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_resolver_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_auth_request_t {
    pub struct_size: u32,
    pub exchange: u32,
    pub stage: u32,
    pub reason_code_present: u8,
    pub properties_present: u8,
    pub method_present: u8,
    pub data_present: u8,
    pub reason_string_present: u8,
    pub reserved: [u8; 3],
    pub reason_code: u32,
    pub generation: u64,
    pub client_id: rumqttc_string_view_t,
    pub method: rumqttc_string_view_t,
    pub auth_method: rumqttc_string_view_t,
    pub data: rumqttc_bytes_view_t,
    pub reason_string: rumqttc_string_view_t,
    pub user_properties: *const rumqttc_user_property_t,
    pub user_property_count: usize,
}

#[repr(C)]
pub struct rumqttc_auth_response_t {
    pub struct_size: u32,
    pub action: u32,
    pub method_present: u8,
    pub data_present: u8,
    pub reason_string_present: u8,
    pub reserved: [u8; 5],
    pub method: rumqttc_string_view_t,
    pub data: rumqttc_bytes_view_t,
    pub reason_string: rumqttc_string_view_t,
    pub user_properties: *const rumqttc_user_property_t,
    pub user_property_count: usize,
    pub reserved_tail: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_auth_vtable_t {
    pub struct_size: u32,
    pub respond: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_auth_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub failed: Option<unsafe extern "C" fn(*mut c_void, *const rumqttc_auth_request_t, u32)>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}

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

pub struct rumqttc_store_registration {
    store: Arc<CStore>,
}

pub struct rumqttc_resolver_registration {
    resolver: Arc<CResolver>,
}

pub struct rumqttc_auth_registration {
    authenticator: Arc<CAuthenticator>,
}

pub struct rumqttc_callback_completion {
    inner: CallbackCompletion,
}

impl Drop for rumqttc_callback_completion {
    fn drop(&mut self) {
        if let CallbackCompletion::Transport(inner) = &self.inner {
            inner.release_host();
        }
        if let CallbackCompletion::WebSocket(inner) = &self.inner {
            inner.release_host();
        }
    }
}

#[derive(Clone)]
enum CallbackCompletion {
    Store(Arc<StoreCompletion>),
    Resolver(Arc<ResolverCompletion>),
    Auth(Arc<AuthCompletion>),
    Transport(Arc<transport::TransportOperation>),
    WebSocket(Arc<websocket::WebSocketCompletion>),
}

struct AuthOwner {
    vtable: rumqttc_auth_vtable_t,
    user_data: usize,
}

unsafe impl Send for AuthOwner {}
unsafe impl Sync for AuthOwner {}

impl Drop for AuthOwner {
    fn drop(&mut self) {
        if let Some(destroy) = self.vtable.destroy {
            unsafe { destroy(self.user_data as *mut c_void) };
        }
    }
}

enum AuthCompletionState {
    Pending(tokio::sync::oneshot::Sender<Result<AuthAction, AuthFailure>>),
    Completed,
    Cancelled,
}

struct AuthCompletion {
    _owner: Arc<AuthOwner>,
    state: Mutex<AuthCompletionState>,
}

impl AuthCompletion {
    fn finish_with(
        &self,
        make_result: impl FnOnce() -> Result<Result<AuthAction, AuthFailure>, u32>,
    ) -> u32 {
        let (sender, result) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(&*state, AuthCompletionState::Pending(sender) if !sender.is_closed()) {
                return crate::error::INVALID_STATE;
            }
            let result = match make_result() {
                Ok(result) => result,
                Err(status) => return status,
            };
            let AuthCompletionState::Pending(sender) =
                std::mem::replace(&mut *state, AuthCompletionState::Completed)
            else {
                unreachable!("pending state checked under lock")
            };
            (sender, result)
        };
        if sender.send(result).is_ok() {
            OK
        } else {
            crate::error::INVALID_STATE
        }
    }

    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, AuthCompletionState::Pending(_)) {
            *state = AuthCompletionState::Cancelled;
        }
    }
}

struct AuthCancelGuard(Arc<AuthCompletion>);
impl Drop for AuthCancelGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct CAuthenticator {
    owner: Arc<AuthOwner>,
}

impl AsyncAuthenticator for CAuthenticator {
    fn respond(&self, context: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
        let owner = self.owner.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let inner = Arc::new(AuthCompletion {
            _owner: owner.clone(),
            state: Mutex::new(AuthCompletionState::Pending(sender)),
        });
        let guard = AuthCancelGuard(inner.clone());
        let (stage, reason_code, properties) = match challenge {
            AsyncAuthChallenge::Start => (1, None, None),
            AsyncAuthChallenge::Continue {
                reason_code,
                properties,
            } => (2, Some(reason_code), properties),
            AsyncAuthChallenge::Success {
                reason_code,
                properties,
            } => (3, Some(reason_code), properties),
        };
        let user_properties: Vec<_> = properties
            .as_ref()
            .map_or(&[][..], |p| p.user_properties.as_slice())
            .iter()
            .map(|(name, value)| rumqttc_user_property_t {
                struct_size: struct_size::<rumqttc_user_property_t>(),
                name: view_string(name),
                value: view_string(value),
            })
            .collect();
        let request = auth_request(
            &context,
            stage,
            reason_code,
            properties.as_ref(),
            &user_properties,
        );
        let completion = rumqttc_callback_completion {
            inner: CallbackCompletion::Auth(inner),
        };
        let callback = owner.vtable.respond.expect("validated auth vtable");
        unsafe {
            callback(
                owner.user_data as *mut c_void,
                &raw const request,
                (&raw const completion).cast_mut(),
            );
        };
        Box::pin(async move {
            let _guard = guard;
            receiver.await.unwrap_or(Err(AuthFailure::Rejected))
        })
    }

    fn failure(&self, context: AuthContext, failure: AuthFailure) {
        let Some(callback) = self.owner.vtable.failed else {
            return;
        };
        let request = auth_request(&context, 4, None, None, &[]);
        unsafe {
            callback(
                self.owner.user_data as *mut c_void,
                &raw const request,
                auth_failure_code(failure),
            );
        };
    }
}

const fn auth_failure_code(failure: AuthFailure) -> u32 {
    match failure {
        AuthFailure::Rejected => 1,
        AuthFailure::Panic => 2,
        AuthFailure::Timeout => 3,
        AuthFailure::InvalidResponse => 4,
        AuthFailure::Overlapping => 5,
        AuthFailure::ConnectionClosed => 6,
        AuthFailure::Method => 7,
        AuthFailure::BrokerRejected => 8,
    }
}

fn auth_request(
    context: &AuthContext,
    stage: u32,
    reason_code: Option<u8>,
    properties: Option<&AuthProperties>,
    user_properties: &[rumqttc_user_property_t],
) -> rumqttc_auth_request_t {
    rumqttc_auth_request_t {
        struct_size: struct_size::<rumqttc_auth_request_t>(),
        exchange: match context.exchange {
            rumqttc_wrapper_core::AuthExchange::Initial => 1,
            rumqttc_wrapper_core::AuthExchange::Reauthentication => 2,
        },
        stage,
        reason_code_present: u8::from(reason_code.is_some()),
        properties_present: u8::from(properties.is_some()),
        method_present: u8::from(properties.and_then(|p| p.method.as_ref()).is_some()),
        data_present: u8::from(properties.and_then(|p| p.data.as_ref()).is_some()),
        reason_string_present: u8::from(
            properties.and_then(|p| p.reason_string.as_ref()).is_some(),
        ),
        reserved: [0; 3],
        reason_code: u32::from(reason_code.unwrap_or(0)),
        generation: context.generation,
        client_id: view_string(&context.client_id),
        method: view_string(&context.method),
        auth_method: properties
            .and_then(|p| p.method.as_deref())
            .map_or(view_string(""), view_string),
        data: properties
            .and_then(|p| p.data.as_ref())
            .map_or(view_bytes(&[]), |data| view_bytes(data)),
        reason_string: properties
            .and_then(|p| p.reason_string.as_deref())
            .map_or(view_string(""), view_string),
        user_properties: if user_properties.is_empty() {
            ptr::null()
        } else {
            user_properties.as_ptr()
        },
        user_property_count: user_properties.len(),
    }
}

struct StoreOwner {
    vtable: rumqttc_store_vtable_t,
    user_data: usize,
}

struct ResolverOwner {
    vtable: rumqttc_resolver_vtable_t,
    user_data: usize,
}

// SAFETY: As with store registration, the application promises that resolver
// callbacks and user_data tolerate overlap across client driver threads.
unsafe impl Send for ResolverOwner {}
unsafe impl Sync for ResolverOwner {}

impl Drop for ResolverOwner {
    fn drop(&mut self) {
        if let Some(destroy) = self.vtable.destroy {
            unsafe { destroy(self.user_data as *mut c_void) };
        }
    }
}

enum ResolverCompletionState {
    Pending(tokio::sync::oneshot::Sender<Result<Vec<SrvRecord>, SrvFailure>>),
    Completed,
    Cancelled,
}

struct ResolverCompletion {
    _owner: Arc<ResolverOwner>,
    state: Mutex<ResolverCompletionState>,
}

impl ResolverCompletion {
    fn finish_with(
        &self,
        make_result: impl FnOnce() -> Result<Result<Vec<SrvRecord>, SrvFailure>, u32>,
    ) -> u32 {
        let (sender, result) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(&*state, ResolverCompletionState::Pending(sender) if !sender.is_closed()) {
                return crate::error::INVALID_STATE;
            }
            let result = match make_result() {
                Ok(result) => result,
                Err(status) => return status,
            };
            let ResolverCompletionState::Pending(sender) =
                std::mem::replace(&mut *state, ResolverCompletionState::Completed)
            else {
                unreachable!("pending state checked under lock")
            };
            (sender, result)
        };
        if sender.send(result).is_ok() {
            OK
        } else {
            crate::error::INVALID_STATE
        }
    }

    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, ResolverCompletionState::Pending(_)) {
            *state = ResolverCompletionState::Cancelled;
        }
    }
}

struct ResolverCancelGuard(Arc<ResolverCompletion>);

impl Drop for ResolverCancelGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct CResolver {
    owner: Arc<ResolverOwner>,
}

impl SrvResolver for CResolver {
    fn resolve(&self, owner_name: String) -> SrvFuture {
        let owner = self.owner.clone();
        Box::pin(async move {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let inner = Arc::new(ResolverCompletion {
                _owner: owner.clone(),
                state: Mutex::new(ResolverCompletionState::Pending(sender)),
            });
            let guard = ResolverCancelGuard(inner.clone());
            {
                let completion = rumqttc_callback_completion {
                    inner: CallbackCompletion::Resolver(inner),
                };
                let request = rumqttc_resolver_request_t {
                    struct_size: struct_size::<rumqttc_resolver_request_t>(),
                    owner: view_string(&owner_name),
                };
                let callback = owner.vtable.resolve.expect("validated resolver vtable");
                unsafe {
                    callback(
                        owner.user_data as *mut c_void,
                        &raw const request,
                        (&raw const completion).cast_mut(),
                    );
                };
            }
            let result = receiver.await.unwrap_or(Err(SrvFailure::Query));
            drop(guard);
            result
        })
    }
}

// SAFETY: Registration requires application callbacks and user_data to be safe
// for calls from different client driver threads. The pointer is never read by
// Rust; it is passed back unchanged and destroyed after all owner references.
unsafe impl Send for StoreOwner {}
unsafe impl Sync for StoreOwner {}

impl Drop for StoreOwner {
    fn drop(&mut self) {
        if let Some(destroy) = self.vtable.destroy {
            unsafe { destroy(self.user_data as *mut c_void) };
        }
    }
}

enum StoreCompletionState {
    Pending(tokio::sync::oneshot::Sender<Result<Option<SessionCheckpoint>, StoreFailure>>),
    Completed,
    Cancelled,
}

struct StoreCompletion {
    _owner: Arc<StoreOwner>,
    operation: u32,
    load_limit: usize,
    state: Mutex<StoreCompletionState>,
}

impl StoreCompletion {
    fn finish_with(
        &self,
        operation: u32,
        make_result: impl FnOnce() -> Result<Result<Option<SessionCheckpoint>, StoreFailure>, u32>,
    ) -> u32 {
        if self.operation != operation {
            return crate::error::INVALID_STATE;
        }
        let (sender, result) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(&*state, StoreCompletionState::Pending(sender) if !sender.is_closed()) {
                return crate::error::INVALID_STATE;
            }
            let result = match make_result() {
                Ok(result) => result,
                Err(status) => return status,
            };
            let StoreCompletionState::Pending(sender) =
                std::mem::replace(&mut *state, StoreCompletionState::Completed)
            else {
                unreachable!("pending state was checked under the same lock")
            };
            (sender, result)
        };
        if sender.send(result).is_ok() {
            OK
        } else {
            crate::error::INVALID_STATE
        }
    }

    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, StoreCompletionState::Pending(_)) {
            *state = StoreCompletionState::Cancelled;
        }
    }
}

struct StoreCancelGuard(Arc<StoreCompletion>);

impl Drop for StoreCancelGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct CStore {
    owner: Arc<StoreOwner>,
}

impl CStore {
    fn invoke(
        &self,
        operation: u32,
        key: SessionStoreKey,
        checkpoint: Option<SessionCheckpoint>,
        load_limit: usize,
    ) -> StoreFuture<Option<SessionCheckpoint>> {
        let owner = self.owner.clone();
        Box::pin(async move {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let inner = Arc::new(StoreCompletion {
                _owner: owner.clone(),
                operation,
                load_limit,
                state: Mutex::new(StoreCompletionState::Pending(sender)),
            });
            let guard = StoreCancelGuard(inner.clone());
            {
                let completion = rumqttc_callback_completion {
                    inner: CallbackCompletion::Store(inner),
                };
                let request = rumqttc_store_request_t {
                    struct_size: struct_size::<rumqttc_store_request_t>(),
                    operation,
                    protocol: match key.protocol {
                        ProtocolVersion::V4 => 1,
                        ProtocolVersion::V5 => 2,
                    },
                    checkpoint_format_version: 1,
                    scope: view_string(&key.scope),
                    client_id: view_string(&key.client_id),
                    checkpoint: checkpoint
                        .as_ref()
                        .map_or(view_bytes(&[]), |value| view_bytes(&value.0)),
                };
                let callback = match operation {
                    1 => owner.vtable.load,
                    2 => owner.vtable.save,
                    _ => owner.vtable.clear,
                }
                .expect("validated store vtable");
                unsafe {
                    callback(
                        owner.user_data as *mut c_void,
                        &raw const request,
                        (&raw const completion).cast_mut(),
                    );
                }
            }
            let result = receiver.await.unwrap_or_else(|_| {
                Err(match operation {
                    1 => StoreFailure::Load,
                    2 => StoreFailure::Save,
                    _ => StoreFailure::Clear,
                })
            });
            drop(guard);
            result
        })
    }
}

impl SessionStore for CStore {
    fn load(&self, key: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
        self.load_with_limit(key, MAX_CHECKPOINT_SIZE)
    }

    fn load_with_limit(
        &self,
        key: SessionStoreKey,
        max_checkpoint_size: usize,
    ) -> StoreFuture<Option<SessionCheckpoint>> {
        self.invoke(1, key, None, max_checkpoint_size)
    }

    fn save(&self, key: SessionStoreKey, checkpoint: SessionCheckpoint) -> StoreFuture<()> {
        let future = self.invoke(2, key, Some(checkpoint), 0);
        Box::pin(async move { future.await.map(|_| ()) })
    }

    fn clear(&self, key: SessionStoreKey) -> StoreFuture<()> {
        let future = self.invoke(3, key, None, 0);
        Box::pin(async move { future.await.map(|_| ()) })
    }
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
                ErrorHandle::panic(format!(
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
    client.ensure_usable().map_err(ErrorHandle::panic)?;
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
        ClientError::Internal(message) => ErrorHandle::internal(message),
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
pub const extern "C" fn rumqttc_library_capabilities() -> u64 {
    CAP_V4
        | CAP_V5
        | CAP_STORE_CALLBACKS
        | CAP_AUTH_CALLBACKS
        | CAP_TRANSPORT_CALLBACKS
        | if cfg!(feature = "websocket") {
            CAP_WEBSOCKET_CALLBACKS
        } else {
            0
        }
        | if cfg!(any(
            feature = "use-rustls-ring",
            feature = "use-rustls-aws-lc"
        )) {
            CAP_RUSTLS
        } else {
            0
        }
        | if cfg!(feature = "use-native-tls") {
            CAP_NATIVE_TLS
        } else {
            0
        }
        | if cfg!(feature = "websocket") {
            CAP_WEBSOCKET
        } else {
            0
        }
        | if cfg!(feature = "http-proxy") {
            CAP_HTTP_PROXY
        } else {
            0
        }
        | if cfg!(feature = "socks-proxy") {
            CAP_SOCKS5_PROXY
        } else {
            0
        }
        | if cfg!(unix) { CAP_UNIX } else { 0 }
        | if cfg!(feature = "system-srv-resolver") {
            CAP_SYSTEM_SRV
        } else {
            0
        }
        | if cfg!(feature = "auth-scram") {
            CAP_SCRAM
        } else {
            0
        }
        | if cfg!(feature = "tracing") {
            CAP_TRACING
        } else {
            0
        }
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
            .shutdown_and_join(Duration::from_millis(timeout_ms))
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
        config.update_with_error(update, || {
            ErrorHandle::internal("configuration lock is poisoned")
        })
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
                config.common.broker = rumqttc_wrapper_core::BrokerTarget::Tcp { host, port };
                if matches!(
                    config.common.transport,
                    rumqttc_wrapper_core::TransportConfig::Unix
                ) {
                    config.common.transport = rumqttc_wrapper_core::TransportConfig::Tcp;
                }
                Ok(())
            })
            .map_err(ErrorHandle::internal)
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
            .map_err(ErrorHandle::internal)
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
            .map_err(ErrorHandle::internal)
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
            .map_err(ErrorHandle::internal)
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
    boundary(error_out, ptr::null_mut(), || {
        if !cfg!(any(
            feature = "use-rustls-ring",
            feature = "use-rustls-aws-lc"
        )) {
            return Err(ErrorHandle::plain(
                crate::error::CONFIG_ERROR,
                1,
                "legacy TLS setter requires the Rustls backend",
            ));
        }
        let ca = unsafe { bytes_from_view(ca) }?.to_vec();
        let certificate = unsafe { bytes_from_view(certificate) }?.to_vec();
        let private_key = unsafe { bytes_from_view(private_key) }?.to_vec();
        let tls = tls_config(ca, certificate, private_key)
            .map_err(|error| ErrorHandle::from_core(&error, None))?;
        unsafe { config_ref(config) }?
            .update(|config| {
                set_transport_tls(config, tls);
                Ok(())
            })
            .map_err(ErrorHandle::internal)
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
            .map_err(ErrorHandle::internal)
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
    boundary(error_out, ptr::null_mut(), || {
        if !cfg!(any(
            feature = "use-rustls-ring",
            feature = "use-rustls-aws-lc"
        )) {
            return Err(ErrorHandle::plain(
                crate::error::CONFIG_ERROR,
                1,
                "legacy WSS setter requires the Rustls backend",
            ));
        }
        let ca = unsafe { bytes_from_view(ca) }?.to_vec();
        let certificate = unsafe { bytes_from_view(certificate) }?.to_vec();
        let private_key = unsafe { bytes_from_view(private_key) }?.to_vec();
        let tls = tls_config(ca, certificate, private_key)
            .map_err(|error| ErrorHandle::from_core(&error, None))?;
        let url = unsafe { string_from_view(url) }?;
        unsafe { config_ref(config) }?
            .update(|config| {
                set_transport_wss(config, url, tls);
                Ok(())
            })
            .map_err(ErrorHandle::internal)
    })
}

unsafe fn parse_tls_options(
    options: *const rumqttc_tls_options_t,
) -> Result<TlsConfig, ErrorHandle> {
    if options.is_null() {
        return Err(ErrorHandle::argument("TLS options are NULL"));
    }
    let options = unsafe { &*options };
    if options.struct_size < struct_size::<rumqttc_tls_options_t>()
        || options.reserved != 0
        || options.reserved_tail != [0; 2]
    {
        return Err(ErrorHandle::argument(
            "invalid TLS options size or reserved fields",
        ));
    }
    let backend = match options.backend {
        0 if cfg!(any(
            feature = "use-rustls-ring",
            feature = "use-rustls-aws-lc"
        )) =>
        {
            TlsBackend::Rustls
        }
        1 if cfg!(feature = "use-native-tls") => TlsBackend::Native,
        0 | 1 => {
            return Err(ErrorHandle::plain(
                crate::error::CONFIG_ERROR,
                1,
                "selected TLS backend is unavailable in this library",
            ));
        }
        _ => return Err(ErrorHandle::argument("unknown TLS backend")),
    };
    let roots = match options.root_policy {
        0 => {
            if options.ca_pem.len != 0 {
                return Err(ErrorHandle::argument(
                    "platform roots cannot include CA PEM data",
                ));
            }
            TlsRootPolicy::Platform
        }
        1 | 2 => {
            let pem = unsafe { bytes_from_view(options.ca_pem) }?;
            if pem.is_empty() {
                return Err(ErrorHandle::argument("custom roots require CA PEM data"));
            }
            if options.root_policy == 1 {
                TlsRootPolicy::Pem(Bytes::copy_from_slice(pem))
            } else {
                TlsRootPolicy::PlatformAndPem(Bytes::copy_from_slice(pem))
            }
        }
        _ => return Err(ErrorHandle::argument("unknown TLS root policy")),
    };
    let identity = match (
        backend,
        options.pem_identity.is_null(),
        options.pkcs12_identity.is_null(),
    ) {
        (_, true, true) => None,
        (TlsBackend::Rustls, false, true) => {
            let raw = unsafe { &*options.pem_identity };
            if raw.struct_size < struct_size::<rumqttc_tls_pem_identity_t>()
                || raw.reserved != 0
                || raw.reserved_tail != [0; 2]
            {
                return Err(ErrorHandle::argument("invalid PEM identity record"));
            }
            let certificate = unsafe { bytes_from_view(raw.certificate) }?;
            let private_key = unsafe { bytes_from_view(raw.private_key) }?;
            if certificate.is_empty() || private_key.is_empty() {
                return Err(ErrorHandle::argument(
                    "PEM certificate and private key must be nonempty",
                ));
            }
            Some(TlsClientIdentity::RustlsPem {
                certificate: Bytes::copy_from_slice(certificate),
                private_key: SecretBytes::new(private_key.to_vec()),
            })
        }
        (TlsBackend::Native, true, false) => {
            let raw = unsafe { &*options.pkcs12_identity };
            if raw.struct_size < struct_size::<rumqttc_tls_pkcs12_identity_t>()
                || raw.reserved != 0
                || raw.reserved_tail != [0; 2]
            {
                return Err(ErrorHandle::argument("invalid PKCS#12 identity record"));
            }
            let identity = unsafe { bytes_from_view(raw.identity) }?;
            let password = unsafe { bytes_from_view(raw.password) }?;
            if identity.is_empty() {
                return Err(ErrorHandle::argument("PKCS#12 identity must be nonempty"));
            }
            Some(TlsClientIdentity::NativePkcs12 {
                identity: SecretBytes::new(identity.to_vec()),
                password: SecretBytes::new(password.to_vec()),
            })
        }
        _ => {
            return Err(ErrorHandle::argument(
                "TLS identity does not match selected backend",
            ));
        }
    };
    if options.alpn_protocol_count > isize::MAX as usize / size_of::<rumqttc_bytes_view_t>() {
        return Err(ErrorHandle::argument("ALPN protocol count is too large"));
    }
    if options.alpn_protocol_count != 0 && options.alpn_protocols.is_null() {
        return Err(ErrorHandle::argument("ALPN protocol pointer is NULL"));
    }
    let views = if options.alpn_protocol_count == 0 {
        &[][..]
    } else {
        unsafe { slice::from_raw_parts(options.alpn_protocols, options.alpn_protocol_count) }
    };
    let mut alpn_protocols = Vec::with_capacity(views.len());
    for view in views {
        let protocol = unsafe { bytes_from_view(*view) }?;
        if protocol.is_empty() || protocol.len() > 255 {
            return Err(ErrorHandle::argument(
                "ALPN identifiers must contain 1 to 255 bytes",
            ));
        }
        if backend == TlsBackend::Native && std::str::from_utf8(protocol).is_err() {
            return Err(ErrorHandle::argument(
                "native TLS ALPN identifiers must be UTF-8",
            ));
        }
        alpn_protocols.push(protocol.to_vec());
    }
    Ok(TlsConfig {
        backend,
        roots,
        identity,
        alpn_protocols,
        ..TlsConfig::default()
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_transport_tls_with_options(
    config: *mut rumqttc_config,
    options: *const rumqttc_tls_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let tls = unsafe { parse_tls_options(options) }?;
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                set_transport_tls(config, tls);
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_transport_wss_with_options(
    config: *mut rumqttc_config,
    url: rumqttc_string_view_t,
    options: *const rumqttc_tls_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if !cfg!(feature = "websocket") {
            return Err(ErrorHandle::plain(
                crate::error::CONFIG_ERROR,
                1,
                "WebSocket is unavailable in this library",
            ));
        }
        let url = unsafe { string_from_view(url) }?;
        let tls = unsafe { parse_tls_options(options) }?;
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                set_transport_wss(config, url, tls);
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

unsafe fn parse_proxy_options(
    options: *const rumqttc_proxy_options_t,
) -> Result<ProxyConfig, ErrorHandle> {
    unsafe { parse_proxy_options_with_tls(options, None) }
}

unsafe fn parse_proxy_options_with_tls(
    options: *const rumqttc_proxy_options_t,
    profile: Option<TlsConfig>,
) -> Result<ProxyConfig, ErrorHandle> {
    if options.is_null() {
        return Err(ErrorHandle::argument("proxy options are NULL"));
    }
    let options = unsafe { &*options };
    if options.struct_size < struct_size::<rumqttc_proxy_options_t>()
        || options.reserved != 0
        || options.reserved_tail != [0; 7]
    {
        return Err(ErrorHandle::argument("invalid proxy options record"));
    }
    if options.dns_policy != 0 {
        return Err(ErrorHandle::argument("unsupported proxy DNS policy"));
    }
    if profile.is_some() && (options.protocol != 2 || !options.tls.is_null()) {
        return Err(ErrorHandle::argument(
            "TLS profile requires HTTPS proxy without legacy TLS options",
        ));
    }
    let host = unsafe { string_from_view(options.host) }?;
    let port = u16::try_from(options.port)
        .map_err(|_| ErrorHandle::argument("proxy port exceeds 65535"))?;
    if port == 0 {
        return Err(ErrorHandle::argument("proxy port must be nonzero"));
    }
    let credentials = if boolean(options.credentials_present, "credentials_present")? {
        let username = unsafe { bytes_from_view(options.username) }?;
        let password = unsafe { bytes_from_view(options.password) }?;
        Some(ProxyCredentials {
            username: std::str::from_utf8(username)
                .map_err(|_| ErrorHandle::argument("proxy username must be UTF-8"))?
                .to_owned(),
            password: std::str::from_utf8(password)
                .map_err(|_| ErrorHandle::argument("proxy password must be UTF-8"))?
                .to_owned(),
        })
    } else {
        if options.username.len != 0 || options.password.len != 0 {
            return Err(ErrorHandle::argument(
                "proxy credentials require presence flag",
            ));
        }
        None
    };
    match options.protocol {
        1 | 2 => {
            if !cfg!(feature = "http-proxy") {
                return Err(ErrorHandle::plain(
                    crate::error::CONFIG_ERROR,
                    1,
                    "HTTP proxy is unavailable in this library",
                ));
            }
            if options.protocol == 1 && !options.tls.is_null() {
                return Err(ErrorHandle::argument(
                    "plain HTTP proxy cannot have TLS options",
                ));
            }
            let tls = if options.protocol == 2 {
                Some(match profile {
                    Some(tls) => tls,
                    None => unsafe { parse_tls_options(options.tls) }?,
                })
            } else {
                None
            };
            Ok(ProxyConfig::Http {
                host,
                port,
                credentials,
                tls,
            })
        }
        3 => {
            if !cfg!(feature = "socks-proxy") {
                return Err(ErrorHandle::plain(
                    crate::error::CONFIG_ERROR,
                    1,
                    "SOCKS5 proxy is unavailable in this library",
                ));
            }
            if !options.tls.is_null() {
                return Err(ErrorHandle::argument(
                    "SOCKS5 proxy cannot have TLS options",
                ));
            }
            Ok(ProxyConfig::Socks5 {
                host,
                port,
                credentials,
            })
        }
        _ => Err(ErrorHandle::argument("unknown proxy protocol")),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_proxy(
    config: *mut rumqttc_config,
    options: *const rumqttc_proxy_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let proxy = unsafe { parse_proxy_options(options) }?;
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                config.common.proxy = Some(proxy);
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_proxy(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.proxy = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_store_registration_new(
    vtable: *const rumqttc_store_vtable_t,
    user_data: *mut c_void,
    out: *mut *mut rumqttc_store_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() || vtable.is_null() {
            return Err(ErrorHandle::argument(
                "store registration output or vtable is NULL",
            ));
        }
        let size = unsafe { (*vtable).struct_size };
        if size < struct_size::<rumqttc_store_vtable_t>() {
            return Err(ErrorHandle::argument("store vtable is too small"));
        }
        let vtable = unsafe { *vtable };
        if vtable.reserved != [0; 2]
            || vtable.load.is_none()
            || vtable.save.is_none()
            || vtable.clear.is_none()
            || vtable.destroy.is_none()
        {
            return Err(ErrorHandle::argument("invalid store vtable"));
        }
        let handle = rumqttc_store_registration {
            store: Arc::new(CStore {
                owner: Arc::new(StoreOwner {
                    vtable,
                    user_data: user_data as usize,
                }),
            }),
        };
        unsafe {
            *out = Box::into_raw(Box::new(handle));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_store_registration_destroy(
    handle: *mut rumqttc_store_registration,
) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_session_store(
    config: *mut rumqttc_config,
    registration: *const rumqttc_store_registration,
    scope: rumqttc_string_view_t,
    timeout_ms: u64,
    max_checkpoint_size: usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if registration.is_null() {
            return Err(ErrorHandle::argument("store registration is NULL"));
        }
        let scope = unsafe { string_from_view(scope) }?;
        if scope.is_empty() || scope.contains('\0') {
            return Err(ErrorHandle::argument(
                "store scope must be nonempty and contain no NUL",
            ));
        }
        if !(8..=MAX_CHECKPOINT_SIZE).contains(&max_checkpoint_size) {
            return Err(ErrorHandle::argument(
                "checkpoint limit must be 8 bytes to 256 MiB",
            ));
        }
        if timeout_ms == 0
            || std::time::Instant::now()
                .checked_add(Duration::from_millis(timeout_ms))
                .is_none()
        {
            return Err(ErrorHandle::argument("invalid store callback timeout"));
        }
        let store_owner = unsafe { &*registration }.store.clone();
        let mut store = SessionStoreConfig::new(store_owner, scope);
        store.timeout = Duration::from_millis(timeout_ms);
        store.max_checkpoint_size = max_checkpoint_size;
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                match &mut config.protocol {
                    rumqttc_wrapper_core::ProtocolConfig::V4(v4) => v4.session_store = Some(store),
                    rumqttc_wrapper_core::ProtocolConfig::V5(v5) => v5.session_store = Some(store),
                }
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_session_store(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        match &mut config.protocol {
            rumqttc_wrapper_core::ProtocolConfig::V4(v4) => v4.session_store = None,
            rumqttc_wrapper_core::ProtocolConfig::V5(v5) => v5.session_store = None,
        }
        Ok(())
    })
}

/// Sets the v4-only policy: 0 rejects, 1 accepts as a fresh session.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v4_session_present_mismatch_policy(
    config: *mut rumqttc_config,
    policy: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let rumqttc_wrapper_core::ProtocolConfig::V4(v4) = &mut config.protocol else {
            return Err(ErrorHandle::argument(
                "session present mismatch policy requires MQTT 3.1.1",
            ));
        };
        v4.session_present_mismatch_policy = match policy {
            0 => rumqttc_wrapper_core::SessionPresentMismatchPolicy::Error,
            1 => rumqttc_wrapper_core::SessionPresentMismatchPolicy::AcceptAsClean,
            _ => {
                return Err(ErrorHandle::argument(
                    "unknown session present mismatch policy",
                ));
            }
        };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_broker_session_resume_policy(
    config: *mut rumqttc_config,
    policy: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol else {
            return Err(ErrorHandle::argument(
                "broker session resume policy requires MQTT 5",
            ));
        };
        v5.broker_session_resume_policy = match policy {
            0 => rumqttc_wrapper_core::BrokerSessionResumePolicy::Strict,
            1 => rumqttc_wrapper_core::BrokerSessionResumePolicy::AllowBrokerOnly,
            _ => {
                return Err(ErrorHandle::argument(
                    "unknown broker session resume policy",
                ));
            }
        };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_redirect_policy(
    config: *mut rumqttc_config,
    policy: u32,
    max_attempts: u32,
    transport: u32,
    tls: *const rumqttc_tls_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let redirect = match policy {
            0 if max_attempts == 0 && transport == 0 && tls.is_null() => {
                rumqttc_wrapper_core::RedirectPolicy::Reject
            }
            1 if max_attempts > 0 => {
                let transport = match transport {
                    0 if tls.is_null() => rumqttc_wrapper_core::TransportConfig::Tcp,
                    1 => rumqttc_wrapper_core::TransportConfig::Tls(unsafe {
                        parse_tls_options(tls)
                    }?),
                    2 if tls.is_null() => rumqttc_wrapper_core::TransportConfig::WebSocket,
                    3 => rumqttc_wrapper_core::TransportConfig::Wss(unsafe {
                        parse_tls_options(tls)
                    }?),
                    _ => {
                        return Err(ErrorHandle::argument(
                            "invalid redirect transport or TLS options",
                        ));
                    }
                };
                rumqttc_wrapper_core::RedirectPolicy::Follow {
                    max_attempts: max_attempts as usize,
                    transport,
                }
            }
            _ => return Err(ErrorHandle::argument("invalid redirect policy")),
        };
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol else {
                    return Err(ErrorHandle::argument("redirect policy requires MQTT 5"));
                };
                v5.redirect_policy = redirect;
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_scram(
    config: *mut rumqttc_config,
    username: rumqttc_string_view_t,
    password: rumqttc_bytes_view_t,
    timeout_ms: u64,
    max_iterations: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if !cfg!(feature = "auth-scram") {
            return Err(ErrorHandle::plain(
                crate::error::CONFIG_ERROR,
                1,
                "SCRAM is unavailable in this library",
            ));
        }
        let username = unsafe { string_from_view(username) }?;
        let password = unsafe { bytes_from_view(password) }?;
        if username.is_empty()
            || std::str::from_utf8(password).is_err()
            || !(4096..=100_000).contains(&max_iterations)
            || timeout_ms == 0
            || std::time::Instant::now()
                .checked_add(Duration::from_millis(timeout_ms))
                .is_none()
        {
            return Err(ErrorHandle::argument("invalid SCRAM settings"));
        }
        let mut scram =
            rumqttc_wrapper_core::ScramConfig::new(username, SecretBytes::new(password.to_vec()));
        scram.exchange_timeout = Duration::from_millis(timeout_ms);
        scram.max_iterations = max_iterations;
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol else {
                    return Err(ErrorHandle::argument("SCRAM requires MQTT 5"));
                };
                if v5.authenticator.is_some() || v5.async_authenticator.is_some() {
                    return Err(ErrorHandle::state("SCRAM conflicts with an authenticator"));
                }
                if v5
                    .connect_properties
                    .authentication_method
                    .as_deref()
                    .is_some_and(|method| method != "SCRAM-SHA-256")
                    || v5.connect_properties.authentication_data.is_some()
                {
                    return Err(ErrorHandle::state(
                        "SCRAM conflicts with CONNECT authentication properties",
                    ));
                }
                v5.connect_properties.authentication_method = Some("SCRAM-SHA-256".into());
                v5.scram = Some(scram);
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_v5_scram(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol else {
            return Err(ErrorHandle::argument("SCRAM requires MQTT 5"));
        };
        if v5.scram.take().is_some()
            && v5.connect_properties.authentication_method.as_deref() == Some("SCRAM-SHA-256")
        {
            v5.connect_properties.authentication_method = None;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_auth_registration_new(
    vtable: *const rumqttc_auth_vtable_t,
    user_data: *mut c_void,
    out: *mut *mut rumqttc_auth_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if vtable.is_null() || out.is_null() {
            return Err(ErrorHandle::argument(
                "authenticator vtable or output is NULL",
            ));
        }
        if unsafe { (*vtable).struct_size } < struct_size::<rumqttc_auth_vtable_t>() {
            return Err(ErrorHandle::argument("authenticator vtable is too small"));
        }
        let vtable = unsafe { *vtable };
        if vtable.reserved != [0; 2] || vtable.respond.is_none() || vtable.destroy.is_none() {
            return Err(ErrorHandle::argument("invalid authenticator vtable"));
        }
        let handle = rumqttc_auth_registration {
            authenticator: Arc::new(CAuthenticator {
                owner: Arc::new(AuthOwner {
                    vtable,
                    user_data: user_data as usize,
                }),
            }),
        };
        unsafe { *out = Box::into_raw(Box::new(handle)) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_auth_registration_destroy(
    registration: *mut rumqttc_auth_registration,
) {
    unsafe { destroy_box(registration) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_authenticator(
    config: *mut rumqttc_config,
    registration: *const rumqttc_auth_registration,
    method: rumqttc_string_view_t,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if registration.is_null() {
            return Err(ErrorHandle::argument("authenticator registration is NULL"));
        }
        if method.len > u16::MAX as usize {
            return Err(ErrorHandle::argument(
                "authentication method exceeds MQTT limit",
            ));
        }
        let method = unsafe { string_from_view(method) }?;
        if method.is_empty()
            || timeout_ms == 0
            || std::time::Instant::now()
                .checked_add(Duration::from_millis(timeout_ms))
                .is_none()
        {
            return Err(ErrorHandle::argument(
                "invalid authentication method or timeout",
            ));
        }
        let authenticator = unsafe { &*registration }.authenticator.clone();
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol else {
                    return Err(ErrorHandle::argument("authenticator requires MQTT 5"));
                };
                if v5.scram.is_some() || v5.authenticator.is_some() {
                    return Err(ErrorHandle::state("another authenticator is configured"));
                }
                if v5.connect_properties.authentication_data.is_some()
                    || v5
                        .connect_properties
                        .authentication_method
                        .as_deref()
                        .is_some_and(|value| value != method)
                {
                    return Err(ErrorHandle::state(
                        "CONNECT authentication properties conflict with the authenticator",
                    ));
                }
                let mut auth = AsyncAuthenticatorConfig::new(authenticator);
                auth.exchange_timeout = Duration::from_millis(timeout_ms);
                auth.configured_method = Some(method.clone());
                v5.connect_properties.authentication_method = Some(method);
                v5.async_authenticator = Some(auth);
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_v5_authenticator(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol else {
            return Err(ErrorHandle::argument("authenticator requires MQTT 5"));
        };
        if let Some(auth) = v5.async_authenticator.take()
            && v5.connect_properties.authentication_method == auth.configured_method
        {
            v5.connect_properties.authentication_method = None;
        }
        Ok(())
    })
}

unsafe fn parse_auth_response(
    response: *const rumqttc_auth_response_t,
) -> Result<Result<AuthAction, AuthFailure>, ErrorHandle> {
    if response.is_null() {
        return Err(ErrorHandle::argument("authentication response is NULL"));
    }
    let response = unsafe { &*response };
    if response.struct_size < struct_size::<rumqttc_auth_response_t>()
        || response.reserved != [0; 5]
        || response.reserved_tail != [0; 2]
    {
        return Err(ErrorHandle::argument(
            "invalid authentication response record",
        ));
    }
    if response.action == 2 {
        if response.method_present != 0
            || response.data_present != 0
            || response.reason_string_present != 0
            || response.user_property_count != 0
            || response.method.len != 0
            || response.data.len != 0
            || response.reason_string.len != 0
        {
            return Err(ErrorHandle::argument(
                "rejected authentication response has properties",
            ));
        }
        return Ok(Err(AuthFailure::Rejected));
    }
    if response.action == 0 {
        if response.method_present != 0
            || response.data_present != 0
            || response.reason_string_present != 0
            || response.user_property_count != 0
            || response.method.len != 0
            || response.data.len != 0
            || response.reason_string.len != 0
        {
            return Err(ErrorHandle::argument(
                "completed authentication response has properties",
            ));
        }
        return Ok(Ok(AuthAction::Complete));
    }
    if response.action != 1 {
        return Err(ErrorHandle::argument("unknown authentication action"));
    }
    let method_present = boolean(response.method_present, "method_present")?;
    let data_present = boolean(response.data_present, "data_present")?;
    let reason_present = boolean(response.reason_string_present, "reason_string_present")?;
    if (!method_present && response.method.len != 0)
        || (!data_present && response.data.len != 0)
        || (!reason_present && response.reason_string.len != 0)
    {
        return Err(ErrorHandle::argument(
            "authentication presence flag is missing",
        ));
    }
    if response.method.len > u16::MAX as usize
        || response.data.len > u16::MAX as usize
        || response.reason_string.len > u16::MAX as usize
    {
        return Err(ErrorHandle::argument(
            "authentication property exceeds MQTT limit",
        ));
    }
    let properties = AuthProperties {
        method: method_present
            .then(|| unsafe { string_from_view(response.method) })
            .transpose()?,
        data: data_present
            .then(|| unsafe { bytes_from_view(response.data) }.map(Bytes::copy_from_slice))
            .transpose()?,
        reason_string: reason_present
            .then(|| unsafe { string_from_view(response.reason_string) })
            .transpose()?,
        user_properties: unsafe {
            parse_user_properties(response.user_properties, response.user_property_count)
        }?,
    };
    Ok(Ok(AuthAction::Send(properties)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_auth_complete(
    completion: *mut rumqttc_callback_completion,
    response: *const rumqttc_auth_response_t,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if completion.is_null() {
            return crate::error::INVALID_ARGUMENT;
        }
        let CallbackCompletion::Auth(inner) = (unsafe { &(*completion).inner }) else {
            return crate::error::INVALID_STATE;
        };
        inner.finish_with(|| unsafe { parse_auth_response(response) }.map_err(|error| error.status))
    }))
    .unwrap_or(crate::error::INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_completion_retain(
    completion: *const rumqttc_callback_completion,
    out: *mut *mut rumqttc_callback_completion,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if completion.is_null() || out.is_null() {
            return Err(ErrorHandle::argument("completion or output is NULL"));
        }
        let inner = unsafe { &(*completion).inner }.clone();
        if let CallbackCompletion::Transport(operation) = &inner {
            operation.retain_host();
        }
        if let CallbackCompletion::WebSocket(operation) = &inner {
            operation.retain_host();
        }
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_callback_completion { inner }));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_completion_destroy(
    completion: *mut rumqttc_callback_completion,
) {
    unsafe { destroy_box(completion) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_store_load_complete(
    completion: *mut rumqttc_callback_completion,
    result: u32,
    checkpoint: rumqttc_bytes_view_t,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        callback_store_load_complete(completion, result, checkpoint)
    }))
    .unwrap_or(crate::error::INTERNAL_ERROR)
}

unsafe fn callback_store_load_complete(
    completion: *mut rumqttc_callback_completion,
    result: u32,
    checkpoint: rumqttc_bytes_view_t,
) -> u32 {
    if completion.is_null() {
        return crate::error::INVALID_ARGUMENT;
    }
    let CallbackCompletion::Store(inner) = (unsafe { &(*completion).inner }) else {
        return crate::error::INVALID_STATE;
    };
    if inner.operation != 1 {
        return crate::error::INVALID_STATE;
    }
    inner.finish_with(1, || match result {
        0 if checkpoint.len > inner.load_limit => Ok(Err(StoreFailure::Oversized)),
        0 => unsafe { bytes_from_view(checkpoint) }
            .map(|bytes| Ok(Some(SessionCheckpoint(Bytes::copy_from_slice(bytes)))))
            .map_err(|_| crate::error::INVALID_ARGUMENT),
        1 if checkpoint.len == 0 => Ok(Ok(None)),
        2 if checkpoint.len == 0 => Ok(Err(StoreFailure::Load)),
        _ => Err(crate::error::INVALID_ARGUMENT),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_store_write_complete(
    completion: *mut rumqttc_callback_completion,
    result: u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        callback_store_write_complete(completion, result)
    }))
    .unwrap_or(crate::error::INTERNAL_ERROR)
}

unsafe fn callback_store_write_complete(
    completion: *mut rumqttc_callback_completion,
    result: u32,
) -> u32 {
    if completion.is_null() {
        return crate::error::INVALID_ARGUMENT;
    }
    let CallbackCompletion::Store(inner) = (unsafe { &(*completion).inner }) else {
        return crate::error::INVALID_STATE;
    };
    let failure = match inner.operation {
        2 => StoreFailure::Save,
        3 => StoreFailure::Clear,
        _ => return crate::error::INVALID_STATE,
    };
    inner.finish_with(inner.operation, || match result {
        0 => Ok(Ok(None)),
        2 => Ok(Err(failure)),
        _ => Err(crate::error::INVALID_ARGUMENT),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_resolver_registration_new(
    vtable: *const rumqttc_resolver_vtable_t,
    user_data: *mut c_void,
    out: *mut *mut rumqttc_resolver_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if vtable.is_null() || out.is_null() {
            return Err(ErrorHandle::argument("resolver vtable or output is NULL"));
        }
        if unsafe { (*vtable).struct_size } < struct_size::<rumqttc_resolver_vtable_t>() {
            return Err(ErrorHandle::argument("resolver vtable is too small"));
        }
        let vtable = unsafe { *vtable };
        if vtable.reserved != [0; 2] || vtable.resolve.is_none() || vtable.destroy.is_none() {
            return Err(ErrorHandle::argument("invalid resolver vtable"));
        }
        let handle = rumqttc_resolver_registration {
            resolver: Arc::new(CResolver {
                owner: Arc::new(ResolverOwner {
                    vtable,
                    user_data: user_data as usize,
                }),
            }),
        };
        unsafe { *out = Box::into_raw(Box::new(handle)) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_resolver_registration_destroy(
    handle: *mut rumqttc_resolver_registration,
) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_srv_resolver(
    config: *mut rumqttc_config,
    registration: *const rumqttc_resolver_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if registration.is_null() {
            return Err(ErrorHandle::argument("resolver registration is NULL"));
        }
        let resolver = unsafe { &*registration }.resolver.clone();
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol else {
                    return Err(ErrorHandle::argument("SRV resolver requires MQTT 5"));
                };
                v5.srv_resolver = Some(SrvResolverConfig(resolver));
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_v5_srv_resolver(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol else {
            return Err(ErrorHandle::argument("SRV resolver requires MQTT 5"));
        };
        v5.srv_resolver = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_srv_complete(
    completion: *mut rumqttc_callback_completion,
    result: u32,
    records: *const rumqttc_srv_record_t,
    record_count: usize,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| unsafe {
        callback_srv_complete(completion, result, records, record_count)
    }))
    .unwrap_or(crate::error::INTERNAL_ERROR)
}

unsafe fn callback_srv_complete(
    completion: *mut rumqttc_callback_completion,
    result: u32,
    records: *const rumqttc_srv_record_t,
    record_count: usize,
) -> u32 {
    if completion.is_null() {
        return crate::error::INVALID_ARGUMENT;
    }
    let CallbackCompletion::Resolver(inner) = (unsafe { &(*completion).inner }) else {
        return crate::error::INVALID_STATE;
    };
    inner.finish_with(|| {
        if result == 2 && record_count == 0 {
            return Ok(Err(SrvFailure::Query));
        }
        if result != 0
            || record_count > isize::MAX as usize / size_of::<rumqttc_srv_record_t>()
            || (record_count != 0 && records.is_null())
        {
            return Err(crate::error::INVALID_ARGUMENT);
        }
        let inputs = if record_count == 0 {
            &[][..]
        } else {
            unsafe { slice::from_raw_parts(records, record_count) }
        };
        let mut output = Vec::with_capacity(inputs.len());
        for record in inputs {
            if record.struct_size < struct_size::<rumqttc_srv_record_t>() || record.reserved != 0 {
                return Err(crate::error::INVALID_ARGUMENT);
            }
            let Ok(priority) = u16::try_from(record.priority) else {
                return Err(crate::error::INVALID_ARGUMENT);
            };
            let Ok(weight) = u16::try_from(record.weight) else {
                return Err(crate::error::INVALID_ARGUMENT);
            };
            let Ok(port) = u16::try_from(record.port) else {
                return Err(crate::error::INVALID_ARGUMENT);
            };
            if port == 0 {
                return Err(crate::error::INVALID_ARGUMENT);
            }
            let target = unsafe { string_from_view(record.target) }
                .map_err(|_| crate::error::INVALID_ARGUMENT)?;
            if target.is_empty() || target.contains(['\0', '\r', '\n']) {
                return Err(crate::error::INVALID_ARGUMENT);
            }
            output.push(SrvRecord {
                priority,
                weight,
                port,
                target,
            });
        }
        Ok(Ok(output))
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
        config.common.incoming_packet_size_limit =
            rumqttc_wrapper_core::IncomingPacketLimit::Bytes(bytes);
        if let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol {
            v5.connect_properties.maximum_packet_size = Some(bytes);
        }
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
pub unsafe extern "C" fn rumqttc_config_set_v4_clean_session(
    config: *mut rumqttc_config,
    clean_session: u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let clean_session = boolean(clean_session, "clean_session");
    config_update(config, error_out, |config| {
        set_v4_clean_session(config, clean_session?).map_err(ErrorHandle::argument)
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

unsafe fn parse_last_will(will: *const rumqttc_last_will_t) -> Result<LastWillConfig, ErrorHandle> {
    if will.is_null() {
        return Err(ErrorHandle::argument("last will is NULL"));
    }
    let will = unsafe { &*will };
    if will.struct_size < struct_size::<rumqttc_last_will_t>() {
        return Err(ErrorHandle::argument("last will struct is too small"));
    }
    if will.reserved != [0; 3] {
        return Err(ErrorHandle::argument(
            "last will reserved fields must be zero",
        ));
    }
    let protocol = match (will.protocol_options, will.v5_properties.is_null()) {
        (PROTOCOL_OPTIONS_VERSION_NEUTRAL, true) => LastWillProtocolOptions::VersionNeutral,
        (PROTOCOL_OPTIONS_V5, false) => {
            let props = unsafe { &*will.v5_properties };
            if props.struct_size < struct_size::<rumqttc_v5_will_properties_t>() {
                return Err(ErrorHandle::argument(
                    "v5 will properties struct is too small",
                ));
            }
            if props.reserved != [0; 2] {
                return Err(ErrorHandle::argument(
                    "v5 will reserved fields must be zero",
                ));
            }
            let payload_format_indicator =
                boolean(props.payload_format_present, "payload_format_present")?
                    .then(|| {
                        u8::try_from(props.payload_format_indicator)
                            .map_err(|_| ErrorHandle::argument("payload format exceeds uint8_t"))
                    })
                    .transpose()?;
            let properties = V5WillProperties {
                will_delay_interval: boolean(props.will_delay_present, "will_delay_present")?
                    .then_some(props.will_delay_interval),
                payload_format_indicator,
                message_expiry_interval: boolean(
                    props.message_expiry_present,
                    "message_expiry_present",
                )?
                .then_some(props.message_expiry_interval),
                content_type: boolean(props.content_type_present, "content_type_present")?
                    .then(|| unsafe { string_from_view(props.content_type) })
                    .transpose()?,
                response_topic: boolean(props.response_topic_present, "response_topic_present")?
                    .then(|| unsafe { string_from_view(props.response_topic) })
                    .transpose()?,
                correlation_data: boolean(
                    props.correlation_data_present,
                    "correlation_data_present",
                )?
                .then(|| {
                    unsafe { bytes_from_view(props.correlation_data) }.map(Bytes::copy_from_slice)
                })
                .transpose()?,
                user_properties: unsafe {
                    parse_user_properties(props.user_properties, props.user_property_count)
                }?,
            };
            LastWillProtocolOptions::V5(properties)
        }
        _ => {
            return Err(ErrorHandle::argument(
                "invalid last will protocol selector or properties",
            ));
        }
    };
    Ok(LastWillConfig {
        topic: unsafe { string_from_view(will.topic) }?,
        payload: Bytes::copy_from_slice(unsafe { bytes_from_view(will.payload) }?),
        qos: qos(will.qos)?,
        retain: boolean(will.retain, "retain")?,
        protocol,
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_last_will(
    config: *mut rumqttc_config,
    will: *const rumqttc_last_will_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let will = unsafe { parse_last_will(will) }?;
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                will.validate(config.protocol_version())
                    .map_err(|e| core_error(&e, None))?;
                config.common.last_will = Some(will);
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_last_will(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.last_will = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_max_request_batch(
    config: *mut rumqttc_config,
    count: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.max_request_batch = count as usize;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_read_batch_size(
    config: *mut rumqttc_config,
    count: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.read_batch_size = count as usize;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_pending_throttle_us(
    config: *mut rumqttc_config,
    microseconds: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.pending_throttle = Duration::from_micros(microseconds);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_local_incoming_packet_limit_bytes(
    config: *mut rumqttc_config,
    bytes: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        if bytes == 0 {
            return Err(ErrorHandle::argument(
                "incoming packet limit must be nonzero",
            ));
        }
        config.common.incoming_packet_size_limit =
            rumqttc_wrapper_core::IncomingPacketLimit::Bytes(bytes);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_local_incoming_packet_limit_mode(
    config: *mut rumqttc_config,
    mode: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.incoming_packet_size_limit = match mode {
            0 => rumqttc_wrapper_core::IncomingPacketLimit::Default,
            1 => rumqttc_wrapper_core::IncomingPacketLimit::Unlimited,
            _ => return Err(ErrorHandle::argument("unknown incoming packet limit mode")),
        };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v4_outgoing_packet_limit_bytes(
    config: *mut rumqttc_config,
    bytes: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let size = usize::try_from(bytes)
            .map_err(|_| ErrorHandle::argument("packet limit exceeds usize"))?;
        if size == 0 {
            return Err(ErrorHandle::argument("packet limit must be nonzero"));
        }
        match &mut config.protocol {
            rumqttc_wrapper_core::ProtocolConfig::V4(v4) => v4.max_outgoing_packet_size = size,
            _ => return Err(ErrorHandle::argument("v4 packet limit requires MQTT 3.1.1")),
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_reset_v4_outgoing_packet_limit(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| match &mut config.protocol {
        rumqttc_wrapper_core::ProtocolConfig::V4(v4) => {
            v4.max_outgoing_packet_size = usize::MAX;
            Ok(())
        }
        _ => Err(ErrorHandle::argument("v4 packet limit requires MQTT 3.1.1")),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v4_inflight_limit(
    config: *mut rumqttc_config,
    limit: u16,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        if limit == 0 {
            return Err(ErrorHandle::argument("inflight limit must be nonzero"));
        }
        match &mut config.protocol {
            rumqttc_wrapper_core::ProtocolConfig::V4(v4) => v4.inflight_limit = limit,
            _ => {
                return Err(ErrorHandle::argument(
                    "v4 inflight limit requires MQTT 3.1.1",
                ));
            }
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_advertised_max_packet_size_bytes(
    config: *mut rumqttc_config,
    bytes: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        if bytes == 0 {
            return Err(ErrorHandle::argument("maximum packet size must be nonzero"));
        }
        match &mut config.protocol {
            rumqttc_wrapper_core::ProtocolConfig::V5(v5) => {
                v5.connect_properties.maximum_packet_size = Some(bytes);
            }
            _ => {
                return Err(ErrorHandle::argument(
                    "advertised packet size requires MQTT 5",
                ));
            }
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_v5_advertised_max_packet_size(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| match &mut config.protocol {
        rumqttc_wrapper_core::ProtocolConfig::V5(v5) => {
            v5.connect_properties.maximum_packet_size = None;
            Ok(())
        }
        _ => Err(ErrorHandle::argument(
            "advertised packet size requires MQTT 5",
        )),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_outgoing_inflight_upper_limit(
    config: *mut rumqttc_config,
    limit: u16,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        if limit == 0 {
            return Err(ErrorHandle::argument(
                "inflight upper limit must be nonzero",
            ));
        }
        match &mut config.protocol {
            rumqttc_wrapper_core::ProtocolConfig::V5(v5) => {
                v5.outgoing_inflight_upper_limit = Some(limit);
            }
            _ => {
                return Err(ErrorHandle::argument(
                    "v5 inflight upper limit requires MQTT 5",
                ));
            }
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_v5_outgoing_inflight_upper_limit(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| match &mut config.protocol {
        rumqttc_wrapper_core::ProtocolConfig::V5(v5) => {
            v5.outgoing_inflight_upper_limit = None;
            Ok(())
        }
        _ => Err(ErrorHandle::argument(
            "v5 inflight upper limit requires MQTT 5",
        )),
    })
}

unsafe fn parse_v5_connect_properties(
    raw: *const rumqttc_v5_connect_properties_t,
) -> Result<V5ConnectProperties, ErrorHandle> {
    if raw.is_null() {
        return Err(ErrorHandle::argument("CONNECT properties are NULL"));
    }
    let raw = unsafe { &*raw };
    if raw.struct_size < struct_size::<rumqttc_v5_connect_properties_t>() {
        return Err(ErrorHandle::argument(
            "CONNECT properties struct is too small",
        ));
    }
    if raw.reserved != [0; 4] || raw.reserved_tail != [0; 2] {
        return Err(ErrorHandle::argument(
            "CONNECT reserved fields must be zero",
        ));
    }
    let receive_maximum = boolean(raw.receive_maximum_present, "receive_maximum_present")?
        .then(|| {
            u16::try_from(raw.receive_maximum)
                .map_err(|_| ErrorHandle::argument("receive maximum exceeds uint16_t"))
        })
        .transpose()?;
    let topic_alias_maximum = boolean(
        raw.topic_alias_maximum_present,
        "topic_alias_maximum_present",
    )?
    .then(|| {
        u16::try_from(raw.topic_alias_maximum)
            .map_err(|_| ErrorHandle::argument("topic alias maximum exceeds uint16_t"))
    })
    .transpose()?;
    let properties = V5ConnectProperties {
        session_expiry_interval: boolean(raw.session_expiry_present, "session_expiry_present")?
            .then_some(raw.session_expiry_interval),
        receive_maximum,
        maximum_packet_size: boolean(
            raw.maximum_packet_size_present,
            "maximum_packet_size_present",
        )?
        .then_some(raw.maximum_packet_size),
        topic_alias_maximum,
        request_response_information: boolean(
            raw.request_response_info_present,
            "request_response_info_present",
        )?
        .then_some(raw.request_response_information),
        request_problem_information: boolean(
            raw.request_problem_info_present,
            "request_problem_info_present",
        )?
        .then_some(raw.request_problem_information),
        authentication_method: boolean(
            raw.authentication_method_present,
            "authentication_method_present",
        )?
        .then(|| unsafe { string_from_view(raw.authentication_method) })
        .transpose()?,
        authentication_data: boolean(
            raw.authentication_data_present,
            "authentication_data_present",
        )?
        .then(|| unsafe { bytes_from_view(raw.authentication_data) }.map(Bytes::copy_from_slice))
        .transpose()?,
        user_properties: unsafe {
            parse_user_properties(raw.user_properties, raw.user_property_count)
        }?,
    };
    properties.validate().map_err(|e| core_error(&e, None))?;
    Ok(properties)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_connect_properties(
    config: *mut rumqttc_config,
    properties: *const rumqttc_v5_connect_properties_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let properties = unsafe { parse_v5_connect_properties(properties) }?;
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| match &mut config.protocol {
                rumqttc_wrapper_core::ProtocolConfig::V5(v5) => {
                    v5.connect_properties = properties;
                    Ok(())
                }
                _ => Err(ErrorHandle::argument("CONNECT properties require MQTT 5")),
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_v5_connect_properties(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| match &mut config.protocol {
        rumqttc_wrapper_core::ProtocolConfig::V5(v5) => {
            v5.connect_properties = rumqttc_wrapper_core::V5Config::default().connect_properties;
            Ok(())
        }
        _ => Err(ErrorHandle::argument("CONNECT properties require MQTT 5")),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_topic_alias_policy(
    config: *mut rumqttc_config,
    policy: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let policy = match policy {
            0 => TopicAliasPolicy::Disabled,
            1 => TopicAliasPolicy::Monotonic,
            2 => TopicAliasPolicy::Lru,
            _ => return Err(ErrorHandle::argument("unknown topic alias policy")),
        };
        match &mut config.protocol {
            rumqttc_wrapper_core::ProtocolConfig::V5(v5) => v5.topic_alias_policy = policy,
            _ => return Err(ErrorHandle::argument("topic alias policy requires MQTT 5")),
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_unix_broker(
    config: *mut rumqttc_config,
    path: rumqttc_bytes_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if !cfg!(unix) {
            return Err(ErrorHandle::plain(
                crate::error::CONFIG_ERROR,
                1,
                "Unix sockets are unsupported on this platform",
            ));
        }
        let path = unsafe { bytes_from_view(path) }?;
        if path.is_empty() || path.contains(&0) {
            return Err(ErrorHandle::argument(
                "Unix socket path must be nonempty and contain no NUL",
            ));
        }
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStringExt;
            std::path::PathBuf::from(std::ffi::OsString::from_vec(path.to_vec()))
        };
        #[cfg(not(unix))]
        let path = std::path::PathBuf::new();
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                config.common.broker = rumqttc_wrapper_core::BrokerTarget::Unix { path };
                config.common.transport = rumqttc_wrapper_core::TransportConfig::Unix;
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_websocket_header_edits(
    config: *mut rumqttc_config,
    edits: *const rumqttc_websocket_header_edit_t,
    count: usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if count > isize::MAX as usize / size_of::<rumqttc_websocket_header_edit_t>() {
            return Err(ErrorHandle::argument("WebSocket header count is too large"));
        }
        if count != 0 && edits.is_null() {
            return Err(ErrorHandle::argument("WebSocket header pointer is NULL"));
        }
        let inputs = if count == 0 {
            &[][..]
        } else {
            unsafe { slice::from_raw_parts(edits, count) }
        };
        let mut parsed = Vec::with_capacity(count);
        for edit in inputs {
            if edit.struct_size < struct_size::<rumqttc_websocket_header_edit_t>()
                || edit.reserved != [0; 2]
            {
                return Err(ErrorHandle::argument("invalid WebSocket header record"));
            }
            let name = unsafe { string_from_view(edit.name) }?;
            let header = match edit.operation {
                0 => WebSocketHeader::Append {
                    name,
                    value: unsafe { string_from_view(edit.value) }?,
                },
                1 => WebSocketHeader::Replace {
                    name,
                    value: unsafe { string_from_view(edit.value) }?,
                },
                2 => {
                    if edit.value.len != 0 {
                        return Err(ErrorHandle::argument(
                            "remove header cannot include a value",
                        ));
                    }
                    WebSocketHeader::Remove { name }
                }
                _ => return Err(ErrorHandle::argument("unknown WebSocket header operation")),
            };
            header.validate().map_err(|e| core_error(&e, None))?;
            parsed.push(header);
        }
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                config.common.websocket_headers = parsed;
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_tcp_send_buffer_size_bytes(
    config: *mut rumqttc_config,
    bytes: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        if bytes == 0 {
            return Err(ErrorHandle::argument(
                "TCP send buffer size must be nonzero",
            ));
        }
        config.common.network.tcp_send_buffer_size = Some(bytes);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_tcp_receive_buffer_size_bytes(
    config: *mut rumqttc_config,
    bytes: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        if bytes == 0 {
            return Err(ErrorHandle::argument(
                "TCP receive buffer size must be nonzero",
            ));
        }
        config.common.network.tcp_receive_buffer_size = Some(bytes);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_tcp_buffer_sizes(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.network.tcp_send_buffer_size = None;
        config.common.network.tcp_receive_buffer_size = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_tcp_nodelay(
    config: *mut rumqttc_config,
    enabled: u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.network.tcp_nodelay = boolean(enabled, "TCP_NODELAY enabled")?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_local_bind_address(
    config: *mut rumqttc_config,
    address: rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let address = unsafe { string_from_view(address) }?
            .parse::<std::net::SocketAddr>()
            .map_err(|_| ErrorHandle::argument("invalid local bind address"))?;
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                config.common.network.local_address = Some(address);
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_local_bind_address(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.network.local_address = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_bind_device(
    config: *mut rumqttc_config,
    device: rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if !cfg!(any(
            target_os = "linux",
            target_os = "android",
            target_os = "fuchsia"
        )) {
            return Err(ErrorHandle::plain(
                crate::error::CONFIG_ERROR,
                1,
                "bind device is unsupported on this platform",
            ));
        }
        let device = unsafe { string_from_view(device) }?;
        if device.is_empty() || device.contains('\0') {
            return Err(ErrorHandle::argument("invalid bind device"));
        }
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                config.common.network.bind_device = Some(device);
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_bind_device(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.network.bind_device = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_mptcp(
    config: *mut rumqttc_config,
    enabled: u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let enabled = boolean(enabled, "MPTCP enabled")?;
        if enabled && !cfg!(target_os = "linux") {
            return Err(ErrorHandle::plain(
                crate::error::CONFIG_ERROR,
                1,
                "MPTCP is unsupported on this platform",
            ));
        }
        config.common.network.mptcp = enabled;
        Ok(())
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
            .map_err(ErrorHandle::internal)?;
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

unsafe fn parse_disconnect_options(
    options: *const rumqttc_disconnect_options_t,
) -> Result<DisconnectProtocolOptions, ErrorHandle> {
    if options.is_null() {
        return Ok(DisconnectProtocolOptions::VersionNeutral);
    }
    let options = unsafe { &*options };
    if options.struct_size < struct_size::<rumqttc_disconnect_options_t>()
        || options.reserved != [0; 2]
    {
        return Err(ErrorHandle::argument("invalid disconnect options record"));
    }
    match (options.protocol_options, options.v5_properties.is_null()) {
        (PROTOCOL_OPTIONS_VERSION_NEUTRAL, true) => Ok(DisconnectProtocolOptions::VersionNeutral),
        (PROTOCOL_OPTIONS_V5, false) => {
            let raw = unsafe { &*options.v5_properties };
            if raw.struct_size < struct_size::<rumqttc_v5_disconnect_properties_t>()
                || raw.reserved != [0; 5]
            {
                return Err(ErrorHandle::argument(
                    "invalid MQTT 5 disconnect properties record",
                ));
            }
            let reason_code = u8::try_from(raw.reason_code)
                .map_err(|_| ErrorHandle::argument("disconnect reason exceeds uint8_t"))?;
            let parsed = V5DisconnectOptions {
                reason_code,
                session_expiry_interval: boolean(
                    raw.session_expiry_present,
                    "session_expiry_present",
                )?
                .then_some(raw.session_expiry_interval),
                reason_string: boolean(raw.reason_string_present, "reason_string_present")?
                    .then(|| unsafe { string_from_view(raw.reason_string) })
                    .transpose()?,
                server_reference: boolean(
                    raw.server_reference_present,
                    "server_reference_present",
                )?
                .then(|| unsafe { string_from_view(raw.server_reference) })
                .transpose()?,
                user_properties: unsafe {
                    parse_user_properties(raw.user_properties, raw.user_property_count)
                }?,
            };
            parsed.validate().map_err(|e| core_error(&e, None))?;
            Ok(DisconnectProtocolOptions::V5(parsed))
        }
        _ => Err(ErrorHandle::argument(
            "invalid disconnect selector or properties",
        )),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_close_with_options_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    options: *const rumqttc_disconnect_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, client, || {
        let options = unsafe { parse_disconnect_options(options) }?;
        let client = unsafe { client_ref(client) }?;
        if matches!(options, DisconnectProtocolOptions::V5(_))
            && client.protocol != ProtocolVersion::V5
        {
            return Err(ErrorHandle::argument(
                "MQTT 5 disconnect options require MQTT 5",
            ));
        }
        client
            .close_with_options(Duration::from_millis(timeout_ms), options)
            .map_err(client_error)?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_close_now_with_options_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    options: *const rumqttc_disconnect_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, client, || {
        let options = unsafe { parse_disconnect_options(options) }?;
        let client = unsafe { client_ref_for_shutdown(client) }?;
        if matches!(options, DisconnectProtocolOptions::V5(_))
            && client.protocol != ProtocolVersion::V5
        {
            return Err(ErrorHandle::argument(
                "MQTT 5 disconnect options require MQTT 5",
            ));
        }
        client
            .close_now_with_options(Duration::from_millis(timeout_ms), options)
            .map_err(client_error)
    })
}

unsafe fn parse_v5_properties(
    properties: *const rumqttc_v5_publish_properties_t,
) -> Result<Option<V5OutgoingPublishProperties>, ErrorHandle> {
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
    Ok(Some(V5OutgoingPublishProperties {
        response_topic,
        correlation_data,
        content_type,
        payload_format_indicator,
        topic_alias,
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
    if count > isize::MAX as usize / std::mem::size_of::<rumqttc_user_property_t>() {
        return Err(ErrorHandle::argument(
            "user-property count exceeds addressable memory",
        ));
    }
    unsafe { slice::from_raw_parts(properties, count) }
        .iter()
        .map(|property| {
            if property.struct_size < struct_size::<rumqttc_user_property_t>() {
                return Err(ErrorHandle::argument("user-property struct is too small"));
            }
            if property.name.len > u16::MAX as usize || property.value.len > u16::MAX as usize {
                return Err(ErrorHandle::argument(
                    "user-property field exceeds MQTT limit",
                ));
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_reauthenticate(
    client: *mut rumqttc_client,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(operation_id_out, 0) };
    boundary(error_out, client, || {
        if operation_id_out.is_null() {
            return Err(ErrorHandle::argument("operation ID output is NULL"));
        }
        write_admission(
            admit(client, Command::Reauthenticate(None))?,
            operation_id_out,
            ptr::null_mut(),
        );
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_reauthenticate_tracked(
    client: *mut rumqttc_client,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(completion_out, ptr::null_mut()) };
    boundary(error_out, client, || {
        if completion_out.is_null() {
            return Err(ErrorHandle::argument("completion output is NULL"));
        }
        write_admission(
            admit(client, Command::Reauthenticate(None))?,
            ptr::null_mut(),
            completion_out,
        );
        Ok(())
    })
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

unsafe fn parse_acknowledgement_options(
    options: *const rumqttc_acknowledgement_options_t,
) -> Result<AcknowledgementProtocolOptions, ErrorHandle> {
    if options.is_null() {
        return Ok(AcknowledgementProtocolOptions::VersionNeutral);
    }
    let options = unsafe { &*options };
    if options.struct_size < struct_size::<rumqttc_acknowledgement_options_t>()
        || options.reserved != [0; 2]
    {
        return Err(ErrorHandle::argument(
            "invalid acknowledgement options record",
        ));
    }
    match (options.protocol_options, options.v5_options.is_null()) {
        (PROTOCOL_OPTIONS_VERSION_NEUTRAL, true) => {
            Ok(AcknowledgementProtocolOptions::VersionNeutral)
        }
        (PROTOCOL_OPTIONS_V5, false) => {
            let raw = unsafe { &*options.v5_options };
            if raw.struct_size < struct_size::<rumqttc_v5_acknowledgement_options_t>()
                || raw.reserved != [0; 7]
                // Each User Property occupies at least five bytes, before all ACK headers.
                || raw.user_property_count > (268_435_455 - 7) / 5
            {
                return Err(ErrorHandle::argument(
                    "invalid MQTT 5 acknowledgement options record",
                ));
            }
            let reason_code = u8::try_from(raw.reason_code)
                .map_err(|_| ErrorHandle::argument("acknowledgement reason exceeds uint8_t"))?;
            let reason_present = boolean(raw.reason_string_present, "reason_string_present")?;
            if reason_present && raw.reason_string.len > usize::from(u16::MAX) {
                return Err(ErrorHandle::argument(
                    "acknowledgement reason string exceeds MQTT limit",
                ));
            }
            let content = V5AcknowledgementOptions {
                reason_code,
                reason_string: reason_present
                    .then(|| unsafe { string_from_view(raw.reason_string) })
                    .transpose()?,
                user_properties: unsafe {
                    parse_user_properties(raw.user_properties, raw.user_property_count)
                }?,
            };
            content.validate().map_err(|e| core_error(&e, None))?;
            Ok(AcknowledgementProtocolOptions::V5(content))
        }
        _ => Err(ErrorHandle::argument(
            "invalid acknowledgement selector or options",
        )),
    }
}

fn acknowledge_impl(
    client: *mut rumqttc_client,
    event: *mut rumqttc_event,
    options: *const rumqttc_acknowledgement_options_t,
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
        let protocol = unsafe { parse_acknowledgement_options(options) }?;
        let mut token = event
            .ack
            .lock()
            .map_err(|_| ErrorHandle::internal("event acknowledgement lock is poisoned"))?;
        let ack = token
            .take()
            .ok_or_else(|| ErrorHandle::state("event has no available acknowledgement"))?;
        let admission = match admit(
            client,
            Command::AcknowledgeWithOptions {
                token: ack,
                protocol,
            },
        ) {
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
    acknowledge_impl(
        client,
        event,
        ptr::null(),
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_acknowledge_tracked(
    client: *mut rumqttc_client,
    event: *mut rumqttc_event,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    acknowledge_impl(
        client,
        event,
        ptr::null(),
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_acknowledge_with_options(
    client: *mut rumqttc_client,
    event: *mut rumqttc_event,
    options: *const rumqttc_acknowledgement_options_t,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    acknowledge_impl(
        client,
        event,
        options,
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_acknowledge_with_options_tracked(
    client: *mut rumqttc_client,
    event: *mut rumqttc_event,
    options: *const rumqttc_acknowledgement_options_t,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    acknowledge_impl(
        client,
        event,
        options,
        ptr::null_mut(),
        completion_out,
        error_out,
    )
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
            return Err(ErrorHandle::would_block("operation is still pending")
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
        Completion::Authenticated => 10,
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
            .ok_or_else(|| ErrorHandle::would_block("completion is not ready"))?;
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
            .ok_or_else(|| ErrorHandle::would_block("completion is not ready"))?;
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
            .ok_or_else(|| ErrorHandle::would_block("completion is not ready"))?;
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
            .ok_or_else(|| ErrorHandle::would_block("completion is not ready"))?;
        let Completion::Diagnostics(value) = terminal else {
            return Err(ErrorHandle::state("completion is not a diagnostics result"));
        };
        fill_diagnostics(&value, unsafe { &mut *out })
    })
}

/// Observes raw and effective CONNACK semantics from a diagnostics completion.
/// Diagnostic codes: 0 none, 1 v4 accepted as clean, 2 v5 broker-only resume.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_connack_session_diagnostics(
    completion: *const rumqttc_completion,
    present_out: *mut u8,
    raw_session_present_out: *mut u8,
    session_resumed_out: *mut u8,
    diagnostic_out: *mut u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        // SAFETY: optional outputs follow the caller's writable-pointer contract.
        unsafe {
            write_optional(present_out, 0);
            write_optional(raw_session_present_out, 0);
            write_optional(session_resumed_out, 0);
            write_optional(diagnostic_out, 0);
        }
        if present_out.is_null()
            && raw_session_present_out.is_null()
            && session_resumed_out.is_null()
            && diagnostic_out.is_null()
        {
            return Err(ErrorHandle::argument(
                "CONNACK diagnostics outputs are NULL",
            ));
        }
        let completion = unsafe { completion_ref(completion) }?;
        let terminal = observe_completion(completion, None)?
            .ok_or_else(|| ErrorHandle::would_block("completion is not ready"))?;
        let Completion::Diagnostics(value) = terminal else {
            return Err(ErrorHandle::state("completion is not a diagnostics result"));
        };
        if let Some(session) = value.connack {
            let diagnostic = match session.diagnostic {
                None => 0,
                Some(
                    rumqttc_wrapper_core::ConnAckDiagnostic::SessionPresentMismatchAcceptedAsClean,
                ) => 1,
                Some(rumqttc_wrapper_core::ConnAckDiagnostic::BrokerOnlySessionResume) => 2,
            };
            // SAFETY: optional outputs follow the caller's writable-pointer contract.
            unsafe {
                write_optional(present_out, 1);
                write_optional(
                    raw_session_present_out,
                    u8::from(session.raw_session_present),
                );
                write_optional(session_resumed_out, u8::from(session.session_resumed));
                write_optional(diagnostic_out, diagnostic);
            }
        }
        Ok(())
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
        WrapperEvent::Authentication(_) => 8,
        WrapperEvent::Redirect(_) => 9,
        WrapperEvent::BrokerDisconnect(_) => 10,
        WrapperEvent::ConnectionRejected(_) => 11,
        WrapperEvent::Disconnected { .. } => 2,
        WrapperEvent::IncomingPublish(_) => 3,
        WrapperEvent::Outgoing(_) => 4,
        WrapperEvent::GracefulShutdownCompleted => 5,
        WrapperEvent::DriverTerminated(_) => 6,
        WrapperEvent::ImmediateShutdownCompleted => 7,
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
            ..
        } = event.event
        else {
            return Err(ErrorHandle::state("event is not a connected event"));
        };
        let protocol = match protocol {
            ProtocolVersion::V4 => 1,
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
pub unsafe extern "C" fn rumqttc_event_connack_reason(
    event: *const rumqttc_event,
    reason_out: *mut u8,
) -> u32 {
    unsafe { write_optional(reason_out, 0) };
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if reason_out.is_null() {
            return Err(ErrorHandle::argument("CONNACK reason output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let details = match &event.event {
            WrapperEvent::Connected { details, .. } | WrapperEvent::ConnectionRejected(details) => {
                details
            }
            _ => return Err(ErrorHandle::state("event has no CONNACK details")),
        };
        unsafe { *reason_out = details.reason_code };
        Ok(())
    })
}

fn connack_details(
    event: &EventObject,
) -> Result<&rumqttc_wrapper_core::ConnAckDetails, ErrorHandle> {
    match &event.event {
        WrapperEvent::Connected { details, .. } | WrapperEvent::ConnectionRejected(details) => {
            Ok(details)
        }
        _ => Err(ErrorHandle::state("event has no CONNACK details")),
    }
}

fn connack_v5(
    event: &EventObject,
) -> Result<&rumqttc_wrapper_core::V5ConnAckProperties, ErrorHandle> {
    connack_details(event)?
        .v5_properties
        .as_deref()
        .ok_or_else(|| ErrorHandle::state("event has no MQTT 5 CONNACK properties"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_connack_v5_scalar(
    event: *const rumqttc_event,
    property: u32,
    present_out: *mut u8,
    value_out: *mut u64,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(value_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument("CONNACK scalar output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let p = connack_v5(event)?;
        let value = match property {
            1 => p.session_expiry_interval.map(u64::from),
            2 => p.receive_maximum.map(u64::from),
            3 => p.maximum_qos.map(u64::from),
            4 => p.retain_available.map(u64::from),
            5 => p.maximum_packet_size.map(u64::from),
            6 => p.topic_alias_maximum.map(u64::from),
            7 => p.wildcard_subscription_available.map(u64::from),
            8 => p.subscription_identifiers_available.map(u64::from),
            9 => p.shared_subscription_available.map(u64::from),
            10 => p.server_keep_alive.map(u64::from),
            _ => return Err(ErrorHandle::argument("unknown CONNACK scalar property")),
        };
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(value_out, value.unwrap_or(0));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_connack_v5_string(
    event: *const rumqttc_event,
    property: u32,
    present_out: *mut u8,
    value_out: *mut rumqttc_string_view_t,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(
            value_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument("CONNACK string output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let p = connack_v5(event)?;
        let value = match property {
            1 => &p.assigned_client_identifier,
            2 => &p.reason_string,
            3 => &p.response_information,
            4 => &p.server_reference,
            5 => &p.authentication_method,
            _ => return Err(ErrorHandle::argument("unknown CONNACK string property")),
        };
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(
                value_out,
                value.as_deref().map_or(
                    rumqttc_string_view_t {
                        data: ptr::null(),
                        len: 0,
                    },
                    view_string,
                ),
            );
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_connack_v5_authentication_data(
    event: *const rumqttc_event,
    present_out: *mut u8,
    value_out: *mut rumqttc_bytes_view_t,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(
            value_out,
            rumqttc_bytes_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument(
                "CONNACK authentication data output is NULL",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let value = &connack_v5(event)?.authentication_data;
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(
                value_out,
                value.as_deref().map_or(
                    rumqttc_bytes_view_t {
                        data: ptr::null(),
                        len: 0,
                    },
                    view_bytes,
                ),
            );
        }
        Ok(())
    })
}

fn event_user_properties(
    event: &EventObject,
    property_class: u32,
) -> Result<&[(String, String)], ErrorHandle> {
    match property_class {
        1 => Ok(&connack_v5(event)?.user_properties),
        2 => match &event.event {
            WrapperEvent::BrokerDisconnect(disconnect) => Ok(&disconnect.user_properties),
            _ => Err(ErrorHandle::state("event is not broker DISCONNECT")),
        },
        3 => match &event.event {
            WrapperEvent::Authentication(auth) => Ok(auth
                .properties
                .as_ref()
                .map_or(&[], |p| p.user_properties.as_slice())),
            _ => Err(ErrorHandle::state("event is not authentication")),
        },
        _ => Err(ErrorHandle::argument("unknown event property class")),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_user_property_count(
    event: *const rumqttc_event,
    property_class: u32,
    count_out: *mut usize,
) -> u32 {
    unsafe { write_optional(count_out, 0) };
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if count_out.is_null() {
            return Err(ErrorHandle::argument("property count output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        unsafe { *count_out = event_user_properties(event, property_class)?.len() };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_user_property_at(
    event: *const rumqttc_event,
    property_class: u32,
    index: usize,
    name_out: *mut rumqttc_string_view_t,
    value_out: *mut rumqttc_string_view_t,
) -> u32 {
    unsafe {
        write_optional(
            name_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
        write_optional(
            value_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if name_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument("property output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let (name, value) = event_user_properties(event, property_class)?
            .get(index)
            .ok_or_else(|| ErrorHandle::argument("property index is out of bounds"))?;
        unsafe {
            write_optional(name_out, view_string(name));
            write_optional(value_out, view_string(value));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_outgoing_packet_id(
    event: *const rumqttc_event,
    present_out: *mut u8,
    packet_id_out: *mut u16,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(packet_id_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && packet_id_out.is_null() {
            return Err(ErrorHandle::argument("packet ID output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::Outgoing(outgoing) = &event.event else {
            return Err(ErrorHandle::state("event is not outgoing activity"));
        };
        unsafe {
            write_optional(present_out, u8::from(outgoing.packet_id.is_some()));
            write_optional(packet_id_out, outgoing.packet_id.unwrap_or(0));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_authentication(
    event: *const rumqttc_event,
    exchange_out: *mut u32,
    stage_out: *mut u32,
    failure_present_out: *mut u8,
    failure_out: *mut u32,
    method_out: *mut rumqttc_string_view_t,
) -> u32 {
    unsafe {
        write_optional(exchange_out, 0);
        write_optional(stage_out, 0);
        write_optional(failure_present_out, 0);
        write_optional(failure_out, 0);
        write_optional(
            method_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if exchange_out.is_null()
            && stage_out.is_null()
            && failure_present_out.is_null()
            && failure_out.is_null()
            && method_out.is_null()
        {
            return Err(ErrorHandle::argument("authentication output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::Authentication(auth) = &event.event else {
            return Err(ErrorHandle::state("event is not authentication"));
        };
        unsafe {
            write_optional(
                exchange_out,
                match auth.exchange {
                    rumqttc_wrapper_core::AuthExchange::Initial => 1,
                    rumqttc_wrapper_core::AuthExchange::Reauthentication => 2,
                },
            );
            write_optional(
                stage_out,
                match auth.stage {
                    rumqttc_wrapper_core::AuthStage::Started => 1,
                    rumqttc_wrapper_core::AuthStage::Continue => 2,
                    rumqttc_wrapper_core::AuthStage::Succeeded => 3,
                    rumqttc_wrapper_core::AuthStage::Failed => 4,
                },
            );
            write_optional(failure_present_out, u8::from(auth.failure.is_some()));
            write_optional(
                failure_out,
                auth.failure.map_or(0, |failure| match failure {
                    rumqttc_wrapper_core::AuthFailure::Rejected => 1,
                    rumqttc_wrapper_core::AuthFailure::Panic => 2,
                    rumqttc_wrapper_core::AuthFailure::Timeout => 3,
                    rumqttc_wrapper_core::AuthFailure::InvalidResponse => 4,
                    rumqttc_wrapper_core::AuthFailure::Overlapping => 5,
                    rumqttc_wrapper_core::AuthFailure::ConnectionClosed => 6,
                    rumqttc_wrapper_core::AuthFailure::Method => 7,
                    rumqttc_wrapper_core::AuthFailure::BrokerRejected => 8,
                }),
            );
            write_optional(method_out, view_string(&auth.method));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_authentication_details(
    event: *const rumqttc_event,
    reason_present_out: *mut u8,
    reason_out: *mut u8,
    properties_present_out: *mut u8,
    method_present_out: *mut u8,
    method_out: *mut rumqttc_string_view_t,
    data_present_out: *mut u8,
    data_out: *mut rumqttc_bytes_view_t,
    reason_string_present_out: *mut u8,
    reason_string_out: *mut rumqttc_string_view_t,
) -> u32 {
    unsafe {
        write_optional(reason_present_out, 0);
        write_optional(reason_out, 0);
        write_optional(properties_present_out, 0);
        write_optional(method_present_out, 0);
        write_optional(
            method_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
        write_optional(data_present_out, 0);
        write_optional(
            data_out,
            rumqttc_bytes_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
        write_optional(reason_string_present_out, 0);
        write_optional(
            reason_string_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if reason_present_out.is_null()
            && reason_out.is_null()
            && properties_present_out.is_null()
            && method_present_out.is_null()
            && method_out.is_null()
            && data_present_out.is_null()
            && data_out.is_null()
            && reason_string_present_out.is_null()
            && reason_string_out.is_null()
        {
            return Err(ErrorHandle::argument(
                "authentication details output is NULL",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::Authentication(auth) = &event.event else {
            return Err(ErrorHandle::state("event is not authentication"));
        };
        unsafe {
            write_optional(reason_present_out, u8::from(auth.reason_code.is_some()));
            write_optional(reason_out, auth.reason_code.unwrap_or(0));
            write_optional(properties_present_out, u8::from(auth.properties.is_some()));
        }
        if let Some(properties) = &auth.properties {
            unsafe {
                write_optional(method_present_out, u8::from(properties.method.is_some()));
                write_optional(
                    method_out,
                    properties.method.as_deref().map_or(
                        rumqttc_string_view_t {
                            data: ptr::null(),
                            len: 0,
                        },
                        view_string,
                    ),
                );
                write_optional(data_present_out, u8::from(properties.data.is_some()));
                write_optional(
                    data_out,
                    properties.data.as_deref().map_or(
                        rumqttc_bytes_view_t {
                            data: ptr::null(),
                            len: 0,
                        },
                        view_bytes,
                    ),
                );
                write_optional(
                    reason_string_present_out,
                    u8::from(properties.reason_string.is_some()),
                );
                write_optional(
                    reason_string_out,
                    properties.reason_string.as_deref().map_or(
                        rumqttc_string_view_t {
                            data: ptr::null(),
                            len: 0,
                        },
                        view_string,
                    ),
                );
            }
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_broker_disconnect(
    event: *const rumqttc_event,
    reason_out: *mut u8,
    expiry_present_out: *mut u8,
    expiry_seconds_out: *mut u32,
    reason_string_present_out: *mut u8,
    reason_string_out: *mut rumqttc_string_view_t,
    server_reference_present_out: *mut u8,
    server_reference_out: *mut rumqttc_string_view_t,
) -> u32 {
    unsafe {
        write_optional(reason_out, 0);
        write_optional(expiry_present_out, 0);
        write_optional(expiry_seconds_out, 0);
        write_optional(reason_string_present_out, 0);
        write_optional(
            reason_string_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
        write_optional(server_reference_present_out, 0);
        write_optional(
            server_reference_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if reason_out.is_null()
            && expiry_present_out.is_null()
            && expiry_seconds_out.is_null()
            && reason_string_present_out.is_null()
            && reason_string_out.is_null()
            && server_reference_present_out.is_null()
            && server_reference_out.is_null()
        {
            return Err(ErrorHandle::argument("broker DISCONNECT output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::BrokerDisconnect(disconnect) = &event.event else {
            return Err(ErrorHandle::state("event is not broker DISCONNECT"));
        };
        unsafe {
            write_optional(reason_out, disconnect.reason_code);
            write_optional(
                expiry_present_out,
                u8::from(disconnect.session_expiry_interval.is_some()),
            );
            write_optional(
                expiry_seconds_out,
                disconnect.session_expiry_interval.unwrap_or(0),
            );
            write_optional(
                reason_string_present_out,
                u8::from(disconnect.reason_string.is_some()),
            );
            write_optional(
                reason_string_out,
                disconnect.reason_string.as_deref().map_or(
                    rumqttc_string_view_t {
                        data: ptr::null(),
                        len: 0,
                    },
                    view_string,
                ),
            );
            write_optional(
                server_reference_present_out,
                u8::from(disconnect.server_reference.is_some()),
            );
            write_optional(
                server_reference_out,
                disconnect.server_reference.as_deref().map_or(
                    rumqttc_string_view_t {
                        data: ptr::null(),
                        len: 0,
                    },
                    view_string,
                ),
            );
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_redirect(
    event: *const rumqttc_event,
    source_out: *mut u32,
    reason_out: *mut u32,
    failure_present_out: *mut u8,
    failure_out: *mut u32,
    reference_present_out: *mut u8,
    reference_out: *mut rumqttc_string_view_t,
) -> u32 {
    unsafe {
        write_optional(source_out, 0);
        write_optional(reason_out, 0);
        write_optional(failure_present_out, 0);
        write_optional(failure_out, 0);
        write_optional(reference_present_out, 0);
        write_optional(
            reference_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if source_out.is_null()
            && reason_out.is_null()
            && failure_present_out.is_null()
            && failure_out.is_null()
            && reference_present_out.is_null()
            && reference_out.is_null()
        {
            return Err(ErrorHandle::argument("redirect output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::Redirect(redirect) = &event.event else {
            return Err(ErrorHandle::state("event is not a redirect"));
        };
        unsafe {
            write_optional(
                source_out,
                match redirect.source {
                    rumqttc_wrapper_core::RedirectSource::ConnAck => 1,
                    rumqttc_wrapper_core::RedirectSource::Disconnect => 2,
                },
            );
            write_optional(
                reason_out,
                match redirect.reason {
                    rumqttc_wrapper_core::RedirectReason::UseAnotherServer => 1,
                    rumqttc_wrapper_core::RedirectReason::ServerMoved => 2,
                },
            );
            write_optional(failure_present_out, u8::from(redirect.failure.is_some()));
            write_optional(
                failure_out,
                redirect.failure.map_or(0, |failure| match failure {
                    rumqttc_wrapper_core::RedirectFailure::Callback(_) => 1,
                    rumqttc_wrapper_core::RedirectFailure::Disabled => 2,
                    rumqttc_wrapper_core::RedirectFailure::Rejected => 3,
                    rumqttc_wrapper_core::RedirectFailure::InvalidReference => 4,
                    rumqttc_wrapper_core::RedirectFailure::UnsupportedTarget => 5,
                    rumqttc_wrapper_core::RedirectFailure::Loop => 6,
                    rumqttc_wrapper_core::RedirectFailure::AttemptLimit => 7,
                    rumqttc_wrapper_core::RedirectFailure::Dns => 8,
                    rumqttc_wrapper_core::RedirectFailure::Timeout => 9,
                    rumqttc_wrapper_core::RedirectFailure::Transport => 10,
                }),
            );
            write_optional(
                reference_present_out,
                u8::from(redirect.server_reference.is_some()),
            );
            write_optional(
                reference_out,
                redirect.server_reference.as_deref().map_or(
                    rumqttc_string_view_t {
                        data: ptr::null(),
                        len: 0,
                    },
                    view_string,
                ),
            );
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_redirect_target(
    event: *const rumqttc_event,
    present_out: *mut u8,
    kind_out: *mut u32,
    value_out: *mut rumqttc_string_view_t,
    port_out: *mut u16,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(kind_out, 0);
        write_optional(
            value_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
        write_optional(port_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && kind_out.is_null() && value_out.is_null() && port_out.is_null()
        {
            return Err(ErrorHandle::argument("redirect target output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::Redirect(redirect) = &event.event else {
            return Err(ErrorHandle::state("event is not a redirect"));
        };
        let Some(target) = &redirect.target else {
            return Ok(());
        };
        let (kind, value, port) = match target {
            rumqttc_wrapper_core::BrokerTarget::Tcp { host, port } => (1, view_string(host), *port),
            rumqttc_wrapper_core::BrokerTarget::WebSocket { url } => (2, view_string(url), 0),
            rumqttc_wrapper_core::BrokerTarget::Unix { .. } => {
                return Err(ErrorHandle::internal(
                    "redirect selected an unsupported Unix endpoint",
                ));
            }
        };
        unsafe {
            write_optional(present_out, 1);
            write_optional(kind_out, kind);
            write_optional(value_out, value);
            write_optional(port_out, port);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_redirect_diagnostics(
    event: *const rumqttc_event,
    decision_out: *mut u32,
    attempts_out: *mut u64,
    limit_present_out: *mut u8,
    limit_out: *mut u64,
    visited_out: *mut u64,
    loop_out: *mut u8,
    candidate_present_out: *mut u8,
    candidate_index_out: *mut u64,
    candidate_count_out: *mut u64,
) -> u32 {
    unsafe {
        write_optional(decision_out, 0);
        write_optional(attempts_out, 0);
        write_optional(limit_present_out, 0);
        write_optional(limit_out, 0);
        write_optional(visited_out, 0);
        write_optional(loop_out, 0);
        write_optional(candidate_present_out, 0);
        write_optional(candidate_index_out, 0);
        write_optional(candidate_count_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if decision_out.is_null()
            && attempts_out.is_null()
            && limit_present_out.is_null()
            && limit_out.is_null()
            && visited_out.is_null()
            && loop_out.is_null()
            && candidate_present_out.is_null()
            && candidate_index_out.is_null()
            && candidate_count_out.is_null()
        {
            return Err(ErrorHandle::argument("redirect diagnostics output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::Redirect(redirect) = &event.event else {
            return Err(ErrorHandle::state("event is not a redirect"));
        };
        let as_u64 = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
        unsafe {
            write_optional(decision_out, if redirect.followed { 1 } else { 2 });
            write_optional(attempts_out, as_u64(redirect.attempts));
            write_optional(
                limit_present_out,
                u8::from(redirect.attempt_limit.is_some()),
            );
            write_optional(limit_out, redirect.attempt_limit.map_or(0, as_u64));
            write_optional(visited_out, as_u64(redirect.visited_endpoints));
            write_optional(
                loop_out,
                u8::from(redirect.failure == Some(rumqttc_wrapper_core::RedirectFailure::Loop)),
            );
            write_optional(
                candidate_present_out,
                u8::from(redirect.srv_candidate_index.is_some()),
            );
            write_optional(
                candidate_index_out,
                redirect.srv_candidate_index.map_or(0, as_u64),
            );
            write_optional(
                candidate_count_out,
                redirect.srv_candidate_count.map_or(0, as_u64),
            );
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
            .map_err(|_| ErrorHandle::internal("event acknowledgement lock is poisoned"))?
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

fn v5_properties(event: &EventObject) -> Result<&V5IncomingPublishProperties, ErrorHandle> {
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
        let kind = match activity.activity {
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
pub unsafe extern "C" fn rumqttc_error_code(
    error: *const rumqttc_error,
    out: *mut rumqttc_string_view_t,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("error code output is NULL"));
        }
        unsafe { *out = view_string(error_ref(error)?.code) };
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

fn error_detail(
    error: *const rumqttc_error,
    present_out: *mut u8,
    detail_out: *mut u32,
    select: impl FnOnce(&ErrorHandle) -> Option<u32>,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(detail_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && detail_out.is_null() {
            return Err(ErrorHandle::argument("error detail output is NULL"));
        }
        let error = unsafe { error_ref(error) }?;
        let value = select(error);
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(detail_out, value.unwrap_or(0));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_store_failure(
    error: *const rumqttc_error,
    present_out: *mut u8,
    failure_out: *mut u32,
) -> u32 {
    error_detail(error, present_out, failure_out, |error| {
        error.store_failure.map(std::num::NonZeroU32::get)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_auth_failure(
    error: *const rumqttc_error,
    present_out: *mut u8,
    failure_out: *mut u32,
) -> u32 {
    error_detail(error, present_out, failure_out, |error| {
        error.auth_failure.map(std::num::NonZeroU32::get)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_redirect_failure(
    error: *const rumqttc_error,
    present_out: *mut u8,
    failure_out: *mut u32,
) -> u32 {
    error_detail(error, present_out, failure_out, |error| {
        error.redirect_failure.map(std::num::NonZeroU32::get)
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_context(
    error: *const rumqttc_error,
    protocol_out: *mut u32,
    phase_out: *mut u32,
    generation_present_out: *mut u8,
    generation_out: *mut u64,
    delivery_status_out: *mut u32,
) -> u32 {
    unsafe {
        write_optional(protocol_out, 0);
        write_optional(phase_out, 0);
        write_optional(generation_present_out, 0);
        write_optional(generation_out, 0);
        write_optional(delivery_status_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if protocol_out.is_null()
            && phase_out.is_null()
            && generation_present_out.is_null()
            && generation_out.is_null()
            && delivery_status_out.is_null()
        {
            return Err(ErrorHandle::argument(
                "at least one error context output is required",
            ));
        }
        let error = unsafe { error_ref(error) }?;
        unsafe {
            write_optional(
                protocol_out,
                error.protocol.map_or(0, std::num::NonZeroU32::get),
            );
            write_optional(phase_out, error.phase.unwrap_or(0));
            write_optional(generation_present_out, u8::from(error.generation.is_some()));
            write_optional(generation_out, error.generation.unwrap_or(0));
            write_optional(delivery_status_out, error.delivery_status);
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
    fn acknowledgement_records_validate_sizes_flags_strings_and_counts() {
        // All pointers refer to live records unless the parser must reject before dereferencing.
        unsafe {
            let mut raw: rumqttc_v5_acknowledgement_options_t = std::mem::zeroed();
            raw.struct_size = struct_size::<rumqttc_v5_acknowledgement_options_t>();
            let mut options = rumqttc_acknowledgement_options_t {
                struct_size: struct_size::<rumqttc_acknowledgement_options_t>(),
                protocol_options: PROTOCOL_OPTIONS_V5,
                v5_options: &raw,
                reserved: [0; 2],
            };
            macro_rules! parse {
                () => {{
                    options.v5_options = &raw const raw;
                    parse_acknowledgement_options(&options)
                }};
            }
            assert!(parse!().is_ok());
            for reason in [0x10, 0xff, 256] {
                raw.reason_code = reason;
                assert!(parse!().is_err());
            }
            raw.reason_code = 0;
            raw.reserved[6] = 1;
            assert!(parse!().is_err());
            raw.reserved[6] = 0;
            raw.struct_size -= 1;
            assert!(parse!().is_err());
            raw.struct_size += 1;
            raw.reason_string_present = 2;
            assert!(parse!().is_err());
            raw.reason_string_present = 1;
            raw.reason_string = rumqttc_string_view_t {
                data: ptr::null(),
                len: 1,
            };
            assert!(parse!().is_err());
            let malformed = [0xffu8];
            raw.reason_string = rumqttc_string_view_t {
                data: malformed.as_ptr().cast(),
                len: 1,
            };
            assert!(parse!().is_err());
            let nul = [0u8];
            raw.reason_string.data = nul.as_ptr().cast();
            assert!(parse!().is_err());
            raw.reason_string = rumqttc_string_view_t {
                data: ptr::dangling(),
                len: 65_536,
            };
            assert!(parse!().is_err());
            raw.reason_string = rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            };
            let AcknowledgementProtocolOptions::V5(parsed) = parse!().unwrap() else {
                panic!()
            };
            assert_eq!(parsed.reason_string.as_deref(), Some(""));
            raw.reason_string_present = 0;
            raw.user_property_count = 1;
            assert!(parse!().is_err());
            raw.user_properties = ptr::dangling();
            raw.user_property_count = usize::MAX;
            assert!(parse!().is_err());
            raw.user_property_count = 0;
            options.reserved[1] = 1;
            assert!(parse!().is_err());
            options.reserved[1] = 0;
            options.struct_size -= 1;
            assert!(parse!().is_err());
            options.struct_size += 1;
            options.protocol_options = 99;
            assert!(parse!().is_err());
            options.protocol_options = PROTOCOL_OPTIONS_VERSION_NEUTRAL;
            assert!(parse!().is_err());
            options.v5_options = ptr::null();
            assert_eq!(
                parse_acknowledgement_options(&options).unwrap(),
                AcknowledgementProtocolOptions::VersionNeutral
            );
            options.protocol_options = PROTOCOL_OPTIONS_V5;
            assert!(parse_acknowledgement_options(&options).is_err());
            assert_eq!(
                parse_acknowledgement_options(ptr::null()).unwrap(),
                AcknowledgementProtocolOptions::VersionNeutral
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires a real TCP broker and driver runtime")]
    fn destroy_after_custom_disconnect_preserves_payload_and_consumes_handle() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        fn packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            let header = byte[0];
            let mut length = 0;
            let mut multiplier = 1;
            loop {
                stream.read_exact(&mut byte).unwrap();
                length += usize::from(byte[0] & 0x7f) * multiplier;
                assert!(length <= 10240);
                if byte[0] & 0x80 == 0 {
                    break;
                }
                multiplier *= 128;
                assert!(multiplier <= 128 * 128 * 128);
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            (header, body)
        }

        for immediate in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                assert_eq!(packet(&mut stream).0, 0x10);
                stream.write_all(b"\x20\x03\x00\x00\x00").unwrap();
                assert_eq!(
                    packet(&mut stream),
                    (0xe0, b"\x00\x0b\x1f\x00\x08selected".to_vec())
                );
                assert_eq!(stream.read(&mut [0]).unwrap(), 0);
            });
            let config = rumqttc_wrapper_core::ClientConfig::v5("ffi-destroy", "127.0.0.1", port);
            let inner = ClientObject::start(config).unwrap();
            assert!(matches!(
                inner.recv(Some(Duration::from_secs(5))),
                Ok(Some(WrapperEvent::Connected { .. }))
            ));
            let client = Box::into_raw(Box::new(rumqttc_client { inner }));
            let properties = rumqttc_v5_disconnect_properties_t {
                struct_size: struct_size::<rumqttc_v5_disconnect_properties_t>(),
                reason_code: 0,
                session_expiry_present: 0,
                reason_string_present: 1,
                server_reference_present: 0,
                reserved: [0; 5],
                session_expiry_interval: 0,
                reason_string: view_string("selected"),
                server_reference: view_string(""),
                user_properties: ptr::null(),
                user_property_count: 0,
            };
            let options = rumqttc_disconnect_options_t {
                struct_size: struct_size::<rumqttc_disconnect_options_t>(),
                protocol_options: PROTOCOL_OPTIONS_V5,
                v5_properties: &raw const properties,
                reserved: [0; 2],
            };
            let status = unsafe {
                if immediate {
                    rumqttc_client_close_now_with_options_timeout_ms(
                        client,
                        5000,
                        &raw const options,
                        ptr::null_mut(),
                    )
                } else {
                    rumqttc_client_close_with_options_timeout_ms(
                        client,
                        5000,
                        &raw const options,
                        ptr::null_mut(),
                    )
                }
            };
            assert_eq!(status, OK);
            assert_eq!(
                unsafe { rumqttc_client_destroy_timeout_ms(client, 5000, ptr::null_mut()) },
                OK
            );
            broker.join().unwrap();
        }
    }

    #[tokio::test]
    async fn async_authenticator_preserves_fields_and_rejects_duplicate_or_late_completion() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Context {
            token: Mutex<Option<usize>>,
            destroyed: Arc<AtomicUsize>,
        }
        unsafe extern "C" fn respond(
            user_data: *mut c_void,
            request: *const rumqttc_auth_request_t,
            completion: *mut rumqttc_callback_completion,
        ) {
            let context = unsafe { &*(user_data as *const Context) };
            let request = unsafe { &*request };
            assert_eq!(request.exchange, 1);
            assert_eq!(request.stage, 2);
            assert_eq!(request.reason_code, 0x18);
            assert_eq!(request.generation, 7);
            assert_eq!(request.data_present, 1);
            assert_eq!(request.data.len, 3);
            assert_eq!(request.user_property_count, 2);
            let mut token = ptr::null_mut();
            assert_eq!(
                unsafe { rumqttc_callback_completion_retain(completion, &raw mut token) },
                OK
            );
            *context.token.lock().unwrap() = Some(token as usize);
        }
        unsafe extern "C" fn destroy(user_data: *mut c_void) {
            let context = unsafe { Box::from_raw(user_data.cast::<Context>()) };
            context.destroyed.fetch_add(1, Ordering::SeqCst);
        }

        let destroyed = Arc::new(AtomicUsize::new(0));
        let context = Box::into_raw(Box::new(Context {
            token: Mutex::new(None),
            destroyed: destroyed.clone(),
        }));
        let owner = Arc::new(AuthOwner {
            vtable: rumqttc_auth_vtable_t {
                struct_size: struct_size::<rumqttc_auth_vtable_t>(),
                respond: Some(respond),
                failed: None,
                destroy: Some(destroy),
                reserved: [0; 2],
            },
            user_data: context as usize,
        });
        let authenticator = CAuthenticator {
            owner: owner.clone(),
        };
        let auth_context = AuthContext {
            client_id: "client".into(),
            exchange: rumqttc_wrapper_core::AuthExchange::Initial,
            generation: 7,
            method: "custom".into(),
        };
        let challenge = AsyncAuthChallenge::Continue {
            reason_code: 0x18,
            properties: Some(AuthProperties {
                method: Some("custom".into()),
                data: Some(Bytes::from_static(b"abc")),
                reason_string: Some(String::new()),
                user_properties: vec![("k".into(), "1".into()), ("k".into(), "2".into())],
            }),
        };
        let pending = authenticator.respond(auth_context.clone(), challenge.clone());
        let token = unsafe { &*context }.token.lock().unwrap().take().unwrap()
            as *mut rumqttc_callback_completion;
        let response_properties = [
            rumqttc_user_property_t {
                struct_size: struct_size::<rumqttc_user_property_t>(),
                name: view_string("p"),
                value: view_string("1"),
            },
            rumqttc_user_property_t {
                struct_size: struct_size::<rumqttc_user_property_t>(),
                name: view_string("p"),
                value: view_string("2"),
            },
        ];
        let response = rumqttc_auth_response_t {
            struct_size: struct_size::<rumqttc_auth_response_t>(),
            action: 1,
            method_present: 1,
            data_present: 1,
            reason_string_present: 1,
            reserved: [0; 5],
            method: view_string("custom"),
            data: view_bytes(b"reply"),
            reason_string: view_string(""),
            user_properties: response_properties.as_ptr(),
            user_property_count: 2,
            reserved_tail: [0; 2],
        };
        assert_eq!(
            unsafe { rumqttc_callback_auth_complete(token, &raw const response) },
            OK
        );
        assert_eq!(
            unsafe { rumqttc_callback_auth_complete(token, &raw const response) },
            crate::error::INVALID_STATE
        );
        let result = pending.await.unwrap();
        let AuthAction::Send(properties) = result else {
            panic!("expected AUTH response")
        };
        assert_eq!(properties.data.as_deref(), Some(&b"reply"[..]));
        assert_eq!(
            properties.user_properties,
            vec![("p".into(), "1".into()), ("p".into(), "2".into())]
        );
        unsafe { rumqttc_callback_completion_destroy(token) };

        let pending = authenticator.respond(auth_context, challenge);
        let late_token = unsafe { &*context }.token.lock().unwrap().take().unwrap()
            as *mut rumqttc_callback_completion;
        drop(pending);
        assert_eq!(
            unsafe { rumqttc_callback_auth_complete(late_token, &raw const response) },
            crate::error::INVALID_STATE
        );
        drop(authenticator);
        drop(owner);
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
        unsafe { rumqttc_callback_completion_destroy(late_token) };
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn oversized_authentication_field_is_rejected_before_copying() {
        let response = rumqttc_auth_response_t {
            struct_size: struct_size::<rumqttc_auth_response_t>(),
            action: 1,
            method_present: 1,
            data_present: 0,
            reason_string_present: 0,
            reserved: [0; 5],
            method: rumqttc_string_view_t {
                data: ptr::dangling::<c_char>(),
                len: u16::MAX as usize + 1,
            },
            data: view_bytes(&[]),
            reason_string: view_string(""),
            user_properties: ptr::null(),
            user_property_count: 0,
            reserved_tail: [0; 2],
        };
        let error = unsafe { parse_auth_response(&raw const response) }.unwrap_err();
        assert_eq!(error.status, crate::error::INVALID_ARGUMENT);
        let error = unsafe {
            parse_user_properties(ptr::dangling::<rumqttc_user_property_t>(), usize::MAX)
        }
        .unwrap_err();
        assert_eq!(error.status, crate::error::INVALID_ARGUMENT);
    }

    #[test]
    fn auth_and_redirect_accessors_preserve_presence_and_clear_wrong_kind_outputs() {
        let auth = rumqttc_event {
            inner: EventObject::new(WrapperEvent::Authentication(
                rumqttc_wrapper_core::AuthEvent {
                    exchange: rumqttc_wrapper_core::AuthExchange::Initial,
                    method: "custom".into(),
                    stage: rumqttc_wrapper_core::AuthStage::Continue,
                    failure: None,
                    reason_code: Some(0x18),
                    properties: Some(AuthProperties {
                        method: Some("custom".into()),
                        data: Some(Bytes::new()),
                        reason_string: Some(String::new()),
                        user_properties: vec![("a".into(), "1".into()), ("a".into(), "2".into())],
                    }),
                },
            )),
        };
        let mut present = 99;
        let mut reason = 99;
        let mut data = rumqttc_bytes_view_t {
            data: std::ptr::dangling::<u8>(),
            len: 99,
        };
        assert_eq!(
            unsafe {
                rumqttc_event_authentication_details(
                    &raw const auth,
                    &raw mut present,
                    &raw mut reason,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &raw mut data,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            OK
        );
        assert_eq!((present, reason, data.len), (1, 0x18, 0));
        let mut count = 99;
        assert_eq!(
            unsafe { rumqttc_event_user_property_count(&raw const auth, 3, &raw mut count) },
            OK
        );
        assert_eq!(count, 2);

        let redirect = rumqttc_event {
            inner: EventObject::new(WrapperEvent::Redirect(
                rumqttc_wrapper_core::RedirectEvent {
                    source: rumqttc_wrapper_core::RedirectSource::ConnAck,
                    reason: rumqttc_wrapper_core::RedirectReason::ServerMoved,
                    server_reference: Some("example:1883".into()),
                    target: None,
                    failure: Some(rumqttc_wrapper_core::RedirectFailure::Loop),
                    followed: false,
                    attempts: 3,
                    attempt_limit: Some(4),
                    visited_endpoints: 3,
                    srv_candidate_index: None,
                    srv_candidate_count: None,
                },
            )),
        };
        let mut decision = 0;
        let mut attempts = 0;
        let mut looped = 0;
        assert_eq!(
            unsafe {
                rumqttc_event_redirect_diagnostics(
                    &raw const redirect,
                    &raw mut decision,
                    &raw mut attempts,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &raw mut looped,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            OK
        );
        assert_eq!((decision, attempts, looped), (2, 3, 1));
        decision = 99;
        attempts = 99;
        assert_eq!(
            unsafe {
                rumqttc_event_redirect_diagnostics(
                    &raw const auth,
                    &raw mut decision,
                    &raw mut attempts,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            crate::error::INVALID_STATE
        );
        assert_eq!((decision, attempts), (0, 0));
    }

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
    fn proxy_options_preserve_endpoint_credentials_and_separate_tls() {
        let capabilities = rumqttc_library_capabilities();
        let tls = rumqttc_tls_options_t {
            struct_size: struct_size::<rumqttc_tls_options_t>(),
            backend: u32::from(capabilities & CAP_RUSTLS == 0),
            root_policy: 0,
            reserved: 0,
            ca_pem: view_bytes(&[]),
            pem_identity: ptr::null(),
            pkcs12_identity: ptr::null(),
            alpn_protocols: ptr::null(),
            alpn_protocol_count: 0,
            reserved_tail: [0; 2],
        };
        let mut options = rumqttc_proxy_options_t {
            struct_size: struct_size::<rumqttc_proxy_options_t>(),
            protocol: 2,
            dns_policy: 0,
            reserved: 0,
            host: view_string("proxy.local"),
            port: 8443,
            username: view_bytes(b"user"),
            password: view_bytes(b"secret"),
            credentials_present: 1,
            reserved_tail: [0; 7],
            tls: &raw const tls,
        };
        if cfg!(feature = "http-proxy") && capabilities & (CAP_RUSTLS | CAP_NATIVE_TLS) != 0 {
            let parsed = unsafe { parse_proxy_options(&raw const options) }.unwrap();
            assert!(
                matches!(parsed, ProxyConfig::Http { host, port: 8443, credentials: Some(_), tls: Some(_) } if host == "proxy.local")
            );
        } else {
            assert!(unsafe { parse_proxy_options(&raw const options) }.is_err());
        }
        options.dns_policy = 1;
        assert!(unsafe { parse_proxy_options(&raw const options) }.is_err());
        options.dns_policy = 0;
        options.protocol = 99;
        assert!(unsafe { parse_proxy_options(&raw const options) }.is_err());
        options.protocol = 3;
        assert!(unsafe { parse_proxy_options(&raw const options) }.is_err());
    }

    #[tokio::test]
    async fn store_callback_can_complete_later_and_reject_duplicates() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Context {
            token: Mutex<Option<usize>>,
            destroyed: Arc<AtomicUsize>,
        }

        unsafe extern "C" fn load(
            user_data: *mut c_void,
            request: *const rumqttc_store_request_t,
            completion: *mut rumqttc_callback_completion,
        ) {
            let context = unsafe { &*(user_data as *const Context) };
            let request = unsafe { &*request };
            assert_eq!(request.operation, 1);
            assert_eq!(request.checkpoint_format_version, 1);
            let mut retained = ptr::null_mut();
            assert_eq!(
                unsafe { rumqttc_callback_completion_retain(completion, &raw mut retained) },
                OK
            );
            *context.token.lock().unwrap() = Some(retained as usize);
        }
        unsafe extern "C" fn unused(
            _: *mut c_void,
            _: *const rumqttc_store_request_t,
            _: *mut rumqttc_callback_completion,
        ) {
        }
        unsafe extern "C" fn destroy(user_data: *mut c_void) {
            let context = unsafe { Box::from_raw(user_data.cast::<Context>()) };
            context.destroyed.fetch_add(1, Ordering::SeqCst);
        }

        let destroyed = Arc::new(AtomicUsize::new(0));
        let context = Box::into_raw(Box::new(Context {
            token: Mutex::new(None),
            destroyed: destroyed.clone(),
        }));
        let owner = Arc::new(StoreOwner {
            vtable: rumqttc_store_vtable_t {
                struct_size: struct_size::<rumqttc_store_vtable_t>(),
                load: Some(load),
                save: Some(unused),
                clear: Some(unused),
                destroy: Some(destroy),
                reserved: [0; 2],
            },
            user_data: context as usize,
        });
        let store = CStore {
            owner: owner.clone(),
        };
        let key = SessionStoreKey {
            protocol: ProtocolVersion::V4,
            scope: "tenant".into(),
            client_id: "client".into(),
        };
        let checkpoint = view_bytes(b"RMWC\0\x01\x04\0payload");
        let work = tokio::spawn(store.load_with_limit(key.clone(), checkpoint.len));
        let token = loop {
            if let Some(token) = unsafe { &*context }.token.lock().unwrap().take() {
                break token as *mut rumqttc_callback_completion;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(
            unsafe { rumqttc_callback_store_load_complete(token, 0, checkpoint) },
            OK
        );
        assert_eq!(
            unsafe { rumqttc_callback_store_load_complete(token, 0, checkpoint) },
            crate::error::INVALID_STATE
        );
        assert_eq!(
            work.await.unwrap().unwrap().unwrap().0.as_ref(),
            b"RMWC\0\x01\x04\0payload"
        );
        unsafe { rumqttc_callback_completion_destroy(token) };

        let oversized_work = tokio::spawn(store.load_with_limit(key.clone(), checkpoint.len - 1));
        let oversized_token = loop {
            if let Some(token) = unsafe { &*context }.token.lock().unwrap().take() {
                break token as *mut rumqttc_callback_completion;
            }
            tokio::task::yield_now().await;
        };
        // This pointer cannot be read. The length check must win before a slice
        // or allocation is made, even when the same registration has other limits.
        let oversized = rumqttc_bytes_view_t {
            data: std::ptr::dangling::<u8>(),
            len: checkpoint.len,
        };
        assert_eq!(
            unsafe { rumqttc_callback_store_load_complete(oversized_token, 0, oversized) },
            OK
        );
        assert_eq!(oversized_work.await.unwrap(), Err(StoreFailure::Oversized));
        assert_eq!(
            unsafe { rumqttc_callback_store_load_complete(oversized_token, 0, checkpoint) },
            crate::error::INVALID_STATE
        );
        unsafe { rumqttc_callback_completion_destroy(oversized_token) };

        let cancelled_work = tokio::spawn(store.load(key));
        let late_token = loop {
            if let Some(token) = unsafe { &*context }.token.lock().unwrap().take() {
                break token as *mut rumqttc_callback_completion;
            }
            tokio::task::yield_now().await;
        };
        cancelled_work.abort();
        assert!(cancelled_work.await.is_err());
        assert_eq!(
            unsafe { rumqttc_callback_store_load_complete(late_token, 0, checkpoint) },
            crate::error::INVALID_STATE
        );
        drop(store);
        drop(owner);
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
        unsafe { rumqttc_callback_completion_destroy(late_token) };
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn connack_accessors_preserve_presence_order_and_wrong_kind_outputs() {
        let properties = rumqttc_wrapper_core::V5ConnAckProperties {
            reason_string: Some(String::new()),
            receive_maximum: Some(9),
            user_properties: vec![("a".into(), "1".into()), ("a".into(), "2".into())],
            ..Default::default()
        };
        let event = rumqttc_event {
            inner: EventObject::new(WrapperEvent::Connected {
                protocol: ProtocolVersion::V5,
                session_present: false,
                details: rumqttc_wrapper_core::ConnAckDetails {
                    reason_code: 0,
                    v5_properties: Some(Box::new(properties)),
                },
            }),
        };
        let mut present = 99;
        let mut scalar = 99;
        assert_eq!(
            unsafe {
                rumqttc_event_connack_v5_scalar(
                    &raw const event,
                    2,
                    &raw mut present,
                    &raw mut scalar,
                )
            },
            OK
        );
        assert_eq!((present, scalar), (1, 9));
        let mut view = rumqttc_string_view_t {
            data: ptr::null(),
            len: 99,
        };
        assert_eq!(
            unsafe {
                rumqttc_event_connack_v5_string(
                    &raw const event,
                    2,
                    &raw mut present,
                    &raw mut view,
                )
            },
            OK
        );
        assert_eq!((present, view.len), (1, 0));
        let mut count = 0;
        assert_eq!(
            unsafe { rumqttc_event_user_property_count(&raw const event, 1, &raw mut count) },
            OK
        );
        assert_eq!(count, 2);
        let mut name = rumqttc_string_view_t {
            data: ptr::null(),
            len: 0,
        };
        assert_eq!(
            unsafe {
                rumqttc_event_user_property_at(&raw const event, 1, 1, &raw mut name, &raw mut view)
            },
            OK
        );
        assert_eq!(
            unsafe {
                std::str::from_utf8_unchecked(slice::from_raw_parts(view.data.cast(), view.len))
            },
            "2"
        );

        let wrong = rumqttc_event {
            inner: EventObject::new(WrapperEvent::GracefulShutdownCompleted),
        };
        present = 99;
        scalar = 99;
        assert_eq!(
            unsafe {
                rumqttc_event_connack_v5_scalar(
                    &raw const wrong,
                    2,
                    &raw mut present,
                    &raw mut scalar,
                )
            },
            crate::error::INVALID_STATE
        );
        assert_eq!((present, scalar), (0, 0));
    }

    #[tokio::test]
    async fn resolver_callback_preserves_records_and_rejects_late_completion() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Context {
            token: Mutex<Option<usize>>,
            destroyed: Arc<AtomicUsize>,
        }
        unsafe extern "C" fn resolve(
            user_data: *mut c_void,
            request: *const rumqttc_resolver_request_t,
            completion: *mut rumqttc_callback_completion,
        ) {
            let context = unsafe { &*(user_data as *const Context) };
            assert_eq!(
                unsafe { (*request).struct_size },
                struct_size::<rumqttc_resolver_request_t>()
            );
            let mut retained = ptr::null_mut();
            assert_eq!(
                unsafe { rumqttc_callback_completion_retain(completion, &raw mut retained) },
                OK
            );
            *context.token.lock().unwrap() = Some(retained as usize);
        }
        unsafe extern "C" fn destroy(user_data: *mut c_void) {
            let context = unsafe { Box::from_raw(user_data.cast::<Context>()) };
            context.destroyed.fetch_add(1, Ordering::SeqCst);
        }
        let destroyed = Arc::new(AtomicUsize::new(0));
        let context = Box::into_raw(Box::new(Context {
            token: Mutex::new(None),
            destroyed: destroyed.clone(),
        }));
        let resolver = CResolver {
            owner: Arc::new(ResolverOwner {
                vtable: rumqttc_resolver_vtable_t {
                    struct_size: struct_size::<rumqttc_resolver_vtable_t>(),
                    resolve: Some(resolve),
                    destroy: Some(destroy),
                    reserved: [0; 2],
                },
                user_data: context as usize,
            }),
        };
        let work = tokio::spawn(resolver.resolve("_mqtt._tcp.example".into()));
        let token = loop {
            if let Some(token) = unsafe { &*context }.token.lock().unwrap().take() {
                break token as *mut rumqttc_callback_completion;
            }
            tokio::task::yield_now().await;
        };
        let record = rumqttc_srv_record_t {
            struct_size: struct_size::<rumqttc_srv_record_t>(),
            priority: 10,
            weight: 20,
            port: 1883,
            reserved: 0,
            target: view_string("broker.example"),
        };
        assert_eq!(
            unsafe { rumqttc_callback_srv_complete(token, 0, &raw const record, 1) },
            OK
        );
        assert_eq!(
            unsafe { rumqttc_callback_srv_complete(token, 0, &raw const record, 1) },
            crate::error::INVALID_STATE
        );
        assert_eq!(work.await.unwrap().unwrap()[0].target, "broker.example");
        unsafe { rumqttc_callback_completion_destroy(token) };

        let cancelled = tokio::spawn(resolver.resolve("_mqtt._tcp.example".into()));
        let late_token = loop {
            if let Some(token) = unsafe { &*context }.token.lock().unwrap().take() {
                break token as *mut rumqttc_callback_completion;
            }
            tokio::task::yield_now().await;
        };
        cancelled.abort();
        assert!(cancelled.await.is_err());
        assert_eq!(
            unsafe { rumqttc_callback_srv_complete(late_token, 0, &raw const record, 1) },
            crate::error::INVALID_STATE
        );
        drop(resolver);
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
        unsafe { rumqttc_callback_completion_destroy(late_token) };
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn store_registration_reuses_one_core_store_across_c_configs() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        unsafe extern "C" fn noop(
            _: *mut c_void,
            _: *const rumqttc_store_request_t,
            _: *mut rumqttc_callback_completion,
        ) {
        }
        unsafe extern "C" fn destroy(user_data: *mut c_void) {
            let counter = unsafe { Box::from_raw(user_data.cast::<Arc<AtomicUsize>>()) };
            counter.fetch_add(1, Ordering::SeqCst);
        }
        let destroyed = Arc::new(AtomicUsize::new(0));
        let user_data = Box::into_raw(Box::new(destroyed.clone())).cast();
        let vtable = rumqttc_store_vtable_t {
            struct_size: struct_size::<rumqttc_store_vtable_t>(),
            load: Some(noop),
            save: Some(noop),
            clear: Some(noop),
            destroy: Some(destroy),
            reserved: [0; 2],
        };
        let mut registration = ptr::null_mut();
        assert_eq!(
            unsafe {
                rumqttc_store_registration_new(
                    &raw const vtable,
                    user_data,
                    &raw mut registration,
                    ptr::null_mut(),
                )
            },
            OK
        );
        let mut first = ptr::null_mut();
        let mut second = ptr::null_mut();
        assert_eq!(
            unsafe { rumqttc_config_new(1, &raw mut first, ptr::null_mut()) },
            OK
        );
        assert_eq!(
            unsafe { rumqttc_config_new(1, &raw mut second, ptr::null_mut()) },
            OK
        );
        for config in [first, second] {
            assert_eq!(
                unsafe {
                    rumqttc_config_set_session_store(
                        config,
                        registration,
                        view_string("tenant"),
                        100,
                        1024,
                        ptr::null_mut(),
                    )
                },
                OK
            );
        }
        let first_owned = unsafe { &*first }.inner.clone_config().unwrap();
        let second_owned = unsafe { &*second }.inner.clone_config().unwrap();
        let (
            rumqttc_wrapper_core::ProtocolConfig::V4(a),
            rumqttc_wrapper_core::ProtocolConfig::V4(b),
        ) = (&first_owned.protocol, &second_owned.protocol)
        else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(
            &a.session_store.as_ref().unwrap().store,
            &b.session_store.as_ref().unwrap().store
        ));
        unsafe {
            rumqttc_store_registration_destroy(registration);
            rumqttc_config_destroy(first);
            rumqttc_config_destroy(second);
        }
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
        drop(first_owned);
        drop(second_owned);
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
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
    fn tls_options_preserve_backend_roots_identity_and_alpn() {
        let ca = b"custom-ca";
        let alpn = [view_bytes(b"mqtt"), view_bytes(b"mqtt-v5")];
        let pkcs12 = rumqttc_tls_pkcs12_identity_t {
            struct_size: struct_size::<rumqttc_tls_pkcs12_identity_t>(),
            reserved: 0,
            identity: view_bytes(b"identity"),
            password: view_bytes(b"password"),
            reserved_tail: [0; 2],
        };
        let options = rumqttc_tls_options_t {
            struct_size: struct_size::<rumqttc_tls_options_t>(),
            backend: 1,
            root_policy: 1,
            reserved: 0,
            ca_pem: view_bytes(ca),
            pem_identity: ptr::null(),
            pkcs12_identity: &raw const pkcs12,
            alpn_protocols: alpn.as_ptr(),
            alpn_protocol_count: alpn.len(),
            reserved_tail: [0; 2],
        };
        let parsed = unsafe { parse_tls_options(&raw const options) };
        if cfg!(feature = "use-native-tls") {
            let parsed = parsed.unwrap();
            assert_eq!(parsed.backend, TlsBackend::Native);
            assert_eq!(parsed.roots, TlsRootPolicy::Pem(Bytes::from_static(ca)));
            assert_eq!(
                parsed.alpn_protocols,
                vec![b"mqtt".to_vec(), b"mqtt-v5".to_vec()]
            );
            let Some(TlsClientIdentity::NativePkcs12 { identity, password }) = parsed.identity
            else {
                panic!("expected PKCS#12 identity");
            };
            assert_eq!(identity.expose(), b"identity");
            assert_eq!(password.expose(), b"password");
        } else {
            assert!(parsed.is_err());
        }
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
