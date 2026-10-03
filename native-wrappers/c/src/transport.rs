//! C transport owners and deferred operations. Included under the FFI module
//! so every exported entry point uses the existing validation/panic boundary.
use super::{
    Arc, AssertUnwindSafe, Bytes, CallbackCompletion, ErrorHandle, Mutex, OK, ProtocolVersion,
    boundary, bytes_from_view, c_void, catch_unwind, config_ref, config_update, destroy_box,
    error_detail, ptr, rumqttc_bytes_view_t, rumqttc_callback_completion, rumqttc_config,
    rumqttc_error, rumqttc_string_view_t, struct_size, view_string,
};
use rumqttc_wrapper_core::{
    NetworkHandling, TransportConnection, TransportConnector, TransportConnectorConfig,
    TransportFailure, TransportFuture, TransportIo, TransportIoFuture, TransportMode,
    TransportRequest,
};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_transport_vtable_t {
    pub struct_size: u32,
    pub mode: u32,
    pub max_retained_operations: u32,
    pub reserved_flags: u32,
    pub connect: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_transport_connect_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub cancel: Option<unsafe extern "C" fn(*mut c_void, u64)>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_transport_stream_vtable_t {
    pub struct_size: u32,
    pub mode: u32,
    pub perform: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_transport_io_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub cancel: Option<unsafe extern "C" fn(*mut c_void, u64)>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_transport_connect_request_t {
    pub struct_size: u32,
    pub protocol: u32,
    pub generation: u64,
    pub operation_id: u64,
    pub remaining_timeout_ns: u64,
    pub target: rumqttc_string_view_t,
    pub client_id: rumqttc_string_view_t,
    pub send_buffer_present: u8,
    pub receive_buffer_present: u8,
    pub tcp_nodelay: u8,
    pub mptcp: u8,
    pub reserved_flags: [u8; 4],
    pub send_buffer_size: u32,
    pub receive_buffer_size: u32,
    pub local_address: rumqttc_string_view_t,
    pub bind_device: rumqttc_string_view_t,
    pub reserved: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_transport_io_request_t {
    pub struct_size: u32,
    pub operation: u32,
    pub generation: u64,
    pub operation_id: u64,
    pub read_limit: usize,
    pub input: rumqttc_bytes_view_t,
    pub reserved: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_transport_response_t {
    pub struct_size: u32,
    pub result: u32,
    pub stream: *const rumqttc_transport_stream,
    pub network_handling: u32,
    pub reserved_flags: u32,
    pub bytes: rumqttc_bytes_view_t,
    pub count: usize,
    pub reserved: [u64; 2],
}

pub struct rumqttc_transport_registration {
    owner: Arc<CTransport>,
}
pub struct rumqttc_transport_stream {
    owner: Arc<CTransportStream>,
}

struct CTransport {
    vtable: rumqttc_transport_vtable_t,
    user_data: usize,
    mode: TransportMode,
    outstanding: AtomicUsize,
    next_id: AtomicU64,
}
// SAFETY: C callbacks promise thread safety; pointers remain opaque and owners
// survive every invocation and retained operation. No wrapper locks are held.
unsafe impl Send for CTransport {}
unsafe impl Sync for CTransport {}
impl Drop for CTransport {
    fn drop(&mut self) {
        unsafe { self.vtable.destroy.unwrap()(self.user_data as *mut c_void) };
    }
}

struct CTransportStream {
    registration: Arc<CTransport>,
    vtable: rumqttc_transport_stream_vtable_t,
    user_data: usize,
    claimed: AtomicBool,
    connect_id: u64,
    generation: u64,
}
unsafe impl Send for CTransportStream {}
unsafe impl Sync for CTransportStream {}
impl Drop for CTransportStream {
    fn drop(&mut self) {
        unsafe { self.vtable.destroy.unwrap()(self.user_data as *mut c_void) };
    }
}

#[derive(Clone)]
enum Owner {
    Connector(Arc<CTransport>),
    Stream(Arc<CTransportStream>),
}
impl Owner {
    fn registration(&self) -> &Arc<CTransport> {
        match self {
            Self::Connector(owner) => owner,
            Self::Stream(owner) => &owner.registration,
        }
    }
    fn cancel(&self, id: u64) {
        match self {
            Self::Connector(owner) => unsafe {
                owner.vtable.cancel.unwrap()(owner.user_data as *mut c_void, id);
            },
            Self::Stream(owner) => unsafe {
                owner.vtable.cancel.unwrap()(owner.user_data as *mut c_void, id);
            },
        }
    }
}

enum Value {
    Connected(TransportConnection),
    Bytes(Bytes),
    Count(usize),
    Unit,
}
enum State {
    Pending(tokio::sync::oneshot::Sender<Result<Value, TransportFailure>>),
    Completed,
    Cancelled,
}

pub(super) struct TransportOperation {
    owner: Owner,
    state: Mutex<State>,
    hosts: AtomicUsize,
    id: u64,
    kind: u32,
    generation: u64,
    input: Bytes,
    limit: usize,
    needs_network: bool,
}
impl Drop for TransportOperation {
    fn drop(&mut self) {
        self.owner
            .registration()
            .outstanding
            .fetch_sub(1, Ordering::AcqRel);
    }
}
impl TransportOperation {
    pub(super) fn retain_host(&self) {
        self.hosts.fetch_add(1, Ordering::Relaxed);
    }
    pub(super) fn release_host(&self) {
        if self.hosts.fetch_sub(1, Ordering::AcqRel) == 1 {
            // The future's cancellation guard is an observer, not a foreign
            // work owner. Dropping the last host token must wake that observer.
            let _ = self.finish(|| Ok(Err(TransportFailure::Abandoned)));
        }
    }
    fn finish(&self, make: impl FnOnce() -> Result<Result<Value, TransportFailure>, u32>) -> u32 {
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
    fn cancel(&self) {
        let notify = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(*state, State::Pending(_)) {
                *state = State::Cancelled;
                true
            } else {
                false
            }
        };
        if notify {
            self.owner.cancel(self.id);
        }
    }
}
struct Cancel(Arc<TransportOperation>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

type Work =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, TransportFailure>> + Send>>;
#[allow(
    deprecated,
    reason = "Atomic fetch_update is available at the Rust 1.88 MSRV"
)]
fn operation(
    owner: Owner,
    kind: u32,
    generation: u64,
    input: Bytes,
    limit: usize,
    needs_network: bool,
) -> Result<(Arc<TransportOperation>, Work), TransportFailure> {
    let registration = owner.registration();
    let id = registration
        .next_id
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| TransportFailure::ResourceLimit)?
        + 1;
    registration
        .outstanding
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < registration.vtable.max_retained_operations as usize).then_some(n + 1)
        })
        .map_err(|_| TransportFailure::ResourceLimit)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let inner = Arc::new(TransportOperation {
        owner,
        state: Mutex::new(State::Pending(sender)),
        hosts: AtomicUsize::new(1),
        id,
        kind,
        generation,
        input,
        limit,
        needs_network,
    });
    // Construct before returning: even an unpolled future notifies cancellation.
    let guard = Cancel(inner.clone());
    let future = Box::pin(async move {
        let _guard = guard;
        receiver.await.unwrap_or(Err(TransportFailure::Abandoned))
    });
    Ok((inner, future))
}

struct Connector(Arc<CTransport>);
impl TransportConnector for Connector {
    fn connect(&self, context: TransportRequest) -> TransportFuture {
        let network = &context.network;
        let work = operation(
            Owner::Connector(self.0.clone()),
            0,
            context.generation,
            Bytes::new(),
            0,
            *network != rumqttc_wrapper_core::NetworkConfig::default(),
        );
        let (inner, future) = match work {
            Ok(work) => work,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let local = network
            .local_address
            .map(|a| a.to_string())
            .unwrap_or_default();
        let request = rumqttc_transport_connect_request_t {
            struct_size: struct_size::<rumqttc_transport_connect_request_t>(),
            protocol: match context.protocol {
                ProtocolVersion::V4 => 1,
                ProtocolVersion::V5 => 2,
            },
            generation: context.generation,
            operation_id: inner.id,
            remaining_timeout_ns: context
                .deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_nanos()
                .try_into()
                .unwrap_or(u64::MAX),
            target: view_string(&context.target),
            client_id: view_string(&context.client_id),
            send_buffer_present: u8::from(network.tcp_send_buffer_size.is_some()),
            receive_buffer_present: u8::from(network.tcp_receive_buffer_size.is_some()),
            tcp_nodelay: u8::from(network.tcp_nodelay),
            mptcp: u8::from(network.mptcp),
            reserved_flags: [0; 4],
            send_buffer_size: network.tcp_send_buffer_size.unwrap_or(0),
            receive_buffer_size: network.tcp_receive_buffer_size.unwrap_or(0),
            local_address: view_string(&local),
            bind_device: view_string(network.bind_device.as_deref().unwrap_or("")),
            reserved: [0; 2],
        };
        let completion = rumqttc_callback_completion {
            inner: CallbackCompletion::Transport(inner),
        };
        unsafe {
            self.0.vtable.connect.unwrap()(
                self.0.user_data as *mut c_void,
                &raw const request,
                (&raw const completion).cast_mut(),
            );
        };
        Box::pin(async move {
            match future.await? {
                Value::Connected(connection) => Ok(connection),
                _ => Err(TransportFailure::InvalidResult),
            }
        })
    }
}

struct StreamIo(Arc<CTransportStream>);
impl StreamIo {
    fn perform(&self, kind: u32, input: Bytes, limit: usize) -> Work {
        if matches!(kind, 1 | 2) && !(1..=16384).contains(&limit) {
            return Box::pin(async { Err(TransportFailure::InvalidResult) });
        }
        let work = operation(
            Owner::Stream(self.0.clone()),
            kind,
            self.0.generation,
            input,
            limit,
            false,
        );
        let (inner, future) = match work {
            Ok(work) => work,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let request = rumqttc_transport_io_request_t {
            struct_size: struct_size::<rumqttc_transport_io_request_t>(),
            operation: kind,
            generation: inner.generation,
            operation_id: inner.id,
            read_limit: if kind == 1 { limit } else { 0 },
            input: rumqttc_bytes_view_t {
                data: inner.input.as_ptr(),
                len: inner.input.len(),
            },
            reserved: [0; 2],
        };
        let completion = rumqttc_callback_completion {
            inner: CallbackCompletion::Transport(inner),
        };
        unsafe {
            self.0.vtable.perform.unwrap()(
                self.0.user_data as *mut c_void,
                &raw const request,
                (&raw const completion).cast_mut(),
            );
        };
        future
    }
}
impl TransportIo for StreamIo {
    fn read(&self, max: usize) -> TransportIoFuture<Bytes> {
        let work = self.perform(1, Bytes::new(), max);
        Box::pin(async move {
            match work.await.map_err(TransportFailure::into_io)? {
                Value::Bytes(bytes) => Ok(bytes),
                _ => Err(TransportFailure::InvalidResult.into_io()),
            }
        })
    }
    fn write(&self, input: Bytes) -> TransportIoFuture<usize> {
        let limit = input.len();
        let work = self.perform(2, input, limit);
        Box::pin(async move {
            match work.await.map_err(TransportFailure::into_io)? {
                Value::Count(count) => Ok(count),
                _ => Err(TransportFailure::InvalidResult.into_io()),
            }
        })
    }
    fn flush(&self) -> TransportIoFuture<()> {
        self.unit(3)
    }
    fn shutdown(&self) -> TransportIoFuture<()> {
        self.unit(4)
    }
}
impl StreamIo {
    fn unit(&self, kind: u32) -> TransportIoFuture<()> {
        let work = self.perform(kind, Bytes::new(), 0);
        Box::pin(async move {
            match work.await.map_err(TransportFailure::into_io)? {
                Value::Unit => Ok(()),
                _ => Err(TransportFailure::InvalidResult.into_io()),
            }
        })
    }
}

fn mode(mode: u32) -> Result<TransportMode, ErrorHandle> {
    match mode {
        1 => Ok(TransportMode::Base),
        2 => Ok(TransportMode::Established),
        _ => Err(ErrorHandle::argument("invalid transport mode")),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_transport_registration_new(
    vtable: *const rumqttc_transport_vtable_t,
    user_data: *mut c_void,
    out: *mut *mut rumqttc_transport_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if vtable.is_null() || out.is_null() {
            return Err(ErrorHandle::argument("transport vtable or output is NULL"));
        }
        if unsafe { (*vtable).struct_size } < struct_size::<rumqttc_transport_vtable_t>() {
            return Err(ErrorHandle::argument("transport vtable is too small"));
        }
        let table = unsafe { *vtable };
        let mode = mode(table.mode)?;
        if table.reserved_flags != 0
            || table.reserved != [0; 2]
            || table.connect.is_none()
            || table.cancel.is_none()
            || table.destroy.is_none()
            || table.max_retained_operations == 0
            || table.max_retained_operations > 65536
        {
            return Err(ErrorHandle::argument("invalid transport vtable"));
        }
        let owner = Arc::new(CTransport {
            vtable: table,
            user_data: user_data as usize,
            mode,
            outstanding: AtomicUsize::new(0),
            next_id: AtomicU64::new(0),
        });
        unsafe { *out = Box::into_raw(Box::new(rumqttc_transport_registration { owner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_transport_registration_destroy(
    registration: *mut rumqttc_transport_registration,
) {
    unsafe { destroy_box(registration) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_transport_connector(
    config: *mut rumqttc_config,
    registration: *const rumqttc_transport_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if registration.is_null() {
            return Err(ErrorHandle::argument("transport registration is NULL"));
        }
        let owner = unsafe { &*registration }.owner.clone();
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                config.common.connector = Some(TransportConnectorConfig {
                    mode: owner.mode,
                    connector: Arc::new(Connector(owner)),
                });
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_transport_connector(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.connector = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_transport_stream_new(
    completion: *const rumqttc_callback_completion,
    vtable: *const rumqttc_transport_stream_vtable_t,
    user_data: *mut c_void,
    out: *mut *mut rumqttc_transport_stream,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if completion.is_null() || vtable.is_null() || out.is_null() {
            return Err(ErrorHandle::argument(
                "stream completion, vtable, or output is NULL",
            ));
        }
        let CallbackCompletion::Transport(operation) = (unsafe { &(*completion).inner }) else {
            return Err(ErrorHandle::state("not a transport completion"));
        };
        if operation.kind != 0 {
            return Err(ErrorHandle::state("stream requires a connect operation"));
        }
        let registration = operation.owner.registration().clone();
        if unsafe { (*vtable).struct_size } < struct_size::<rumqttc_transport_stream_vtable_t>() {
            return Err(ErrorHandle::argument("stream vtable is too small"));
        }
        let table = unsafe { *vtable };
        if mode(table.mode)? != registration.mode
            || table.reserved != [0; 2]
            || table.perform.is_none()
            || table.cancel.is_none()
            || table.destroy.is_none()
        {
            return Err(ErrorHandle::argument("invalid stream vtable"));
        }
        let owner = Arc::new(CTransportStream {
            registration,
            vtable: table,
            user_data: user_data as usize,
            claimed: AtomicBool::new(false),
            connect_id: operation.id,
            generation: operation.generation,
        });
        unsafe { *out = Box::into_raw(Box::new(rumqttc_transport_stream { owner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_transport_stream_destroy(stream: *mut rumqttc_transport_stream) {
    unsafe { destroy_box(stream) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_transport_complete(
    completion: *mut rumqttc_callback_completion,
    response: *const rumqttc_transport_response_t,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if completion.is_null() {
            return crate::error::INVALID_ARGUMENT;
        }
        let CallbackCompletion::Transport(inner) = (unsafe { &(*completion).inner }) else {
            return crate::error::INVALID_STATE;
        };
        inner.finish(|| unsafe { parse_response(inner, response) })
    }))
    .unwrap_or(crate::error::INTERNAL_ERROR)
}

unsafe fn parse_response(
    inner: &TransportOperation,
    response: *const rumqttc_transport_response_t,
) -> Result<Result<Value, TransportFailure>, u32> {
    let invalid = crate::error::INVALID_ARGUMENT;
    if response.is_null()
        || unsafe { (*response).struct_size } < struct_size::<rumqttc_transport_response_t>()
    {
        return Err(invalid);
    }
    let response = unsafe { &*response };
    if response.reserved_flags != 0 || response.reserved != [0; 2] {
        return Err(invalid);
    }
    if response.result != 0 {
        if !response.stream.is_null()
            || response.bytes.len != 0
            || response.count != 0
            || response.network_handling != 0
        {
            return Err(invalid);
        }
        let failure = match response.result {
            1 => TransportFailure::Connect,
            2 => TransportFailure::NetworkOptions,
            3 => TransportFailure::Composition,
            4 => TransportFailure::InvalidResult,
            5 => TransportFailure::Abandoned,
            6 => TransportFailure::Io,
            7 => TransportFailure::Timeout,
            8 => TransportFailure::Panic,
            9 => TransportFailure::ResourceLimit,
            _ => return Err(invalid),
        };
        return Ok(Err(failure));
    }
    match inner.kind {
        0 => {
            if response.stream.is_null() || response.bytes.len != 0 || response.count != 0 {
                return Err(invalid);
            }
            let handling = match response.network_handling {
                1 => NetworkHandling::Applied,
                2 => NetworkHandling::NotApplicable,
                _ => return Err(invalid),
            };
            if inner.needs_network && handling == NetworkHandling::NotApplicable {
                return Ok(Err(TransportFailure::NetworkOptions));
            }
            let stream = unsafe { &*response.stream }.owner.clone();
            if !Arc::ptr_eq(&stream.registration, inner.owner.registration()) {
                return Err(invalid);
            }
            if (stream.connect_id, stream.generation) != (inner.id, inner.generation) {
                return Err(crate::error::INVALID_STATE);
            }
            if stream
                .claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(crate::error::INVALID_STATE);
            }
            Ok(Ok(Value::Connected(TransportConnection {
                mode: stream.registration.mode,
                io: Arc::new(StreamIo(stream)),
                network_handling: handling,
            })))
        }
        kind => {
            if !response.stream.is_null() || response.network_handling != 0 {
                return Err(invalid);
            }
            match kind {
                1 if response.count != 0 => Err(invalid),
                1 if response.bytes.len > inner.limit => Ok(Err(TransportFailure::InvalidResult)),
                1 => unsafe { bytes_from_view(response.bytes) }
                    .map(|bytes| Ok(Value::Bytes(Bytes::copy_from_slice(bytes))))
                    .map_err(|_| invalid),
                2 if response.bytes.len != 0 => Err(invalid),
                2 if response.count == 0 || response.count > inner.limit => {
                    Ok(Err(TransportFailure::InvalidResult))
                }
                2 => Ok(Ok(Value::Count(response.count))),
                3 | 4 if response.bytes.len == 0 && response.count == 0 => Ok(Ok(Value::Unit)),
                _ => Err(invalid),
            }
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_transport_failure(
    error: *const rumqttc_error,
    present_out: *mut u8,
    failure_out: *mut u32,
) -> u32 {
    error_detail(error, present_out, failure_out, |error| {
        error
            .failure_details()
            .transport
            .map(std::num::NonZeroU32::get)
    })
}

#[cfg(test)]
mod tests {
    use super::super::{rumqttc_callback_completion_destroy, rumqttc_callback_completion_retain};
    use super::*;
    use std::time::{Duration, Instant};
    #[derive(Default)]
    struct Host {
        tokens: Mutex<Vec<usize>>,
        destroyed: AtomicUsize,
        streams_destroyed: AtomicUsize,
        cancellations: AtomicUsize,
    }
    unsafe fn host<'a>(data: *mut c_void) -> &'a Arc<Host> {
        unsafe { &*data.cast::<Arc<Host>>() }
    }
    unsafe extern "C" fn deferred_connect(
        data: *mut c_void,
        _: *const rumqttc_transport_connect_request_t,
        token: *mut rumqttc_callback_completion,
    ) {
        unsafe { retain(data, token) };
    }
    unsafe extern "C" fn deferred_io(
        data: *mut c_void,
        _: *const rumqttc_transport_io_request_t,
        token: *mut rumqttc_callback_completion,
    ) {
        unsafe { retain(data, token) };
    }
    unsafe fn retain(data: *mut c_void, token: *mut rumqttc_callback_completion) {
        let mut retained = ptr::null_mut();
        assert_eq!(
            unsafe { rumqttc_callback_completion_retain(token, &raw mut retained) },
            OK
        );
        unsafe { host(data) }
            .tokens
            .lock()
            .unwrap()
            .push(retained as usize);
    }
    unsafe extern "C" fn cancelled(data: *mut c_void, _: u64) {
        unsafe { host(data) }
            .cancellations
            .fetch_add(1, Ordering::SeqCst);
    }
    unsafe extern "C" fn destroyed(data: *mut c_void) {
        let owner = unsafe { Box::from_raw(data.cast::<Arc<Host>>()) };
        owner.destroyed.fetch_add(1, Ordering::SeqCst);
    }
    unsafe extern "C" fn stream_destroyed(data: *mut c_void) {
        let owner = unsafe { Box::from_raw(data.cast::<Arc<Host>>()) };
        owner.streams_destroyed.fetch_add(1, Ordering::SeqCst);
    }
    fn registration(
        host: &Arc<Host>,
        limit: u32,
    ) -> (Connector, *mut rumqttc_transport_registration) {
        let table = rumqttc_transport_vtable_t {
            struct_size: struct_size::<rumqttc_transport_vtable_t>(),
            mode: 1,
            max_retained_operations: limit,
            reserved_flags: 0,
            connect: Some(deferred_connect),
            cancel: Some(cancelled),
            destroy: Some(destroyed),
            reserved: [0; 2],
        };
        let mut handle = ptr::null_mut();
        let data = Box::into_raw(Box::new(host.clone())).cast();
        assert_eq!(
            unsafe {
                rumqttc_transport_registration_new(
                    &raw const table,
                    data,
                    &raw mut handle,
                    ptr::null_mut(),
                )
            },
            OK
        );
        (Connector(unsafe { &*handle }.owner.clone()), handle)
    }
    fn token(host: &Host) -> *mut rumqttc_callback_completion {
        host.tokens.lock().unwrap().remove(0) as *mut _
    }
    fn request(generation: u64) -> TransportRequest {
        TransportRequest {
            protocol: ProtocolVersion::V4,
            client_id: "transport-test".into(),
            target: "supplied.invalid:1883".into(),
            generation,
            deadline: Instant::now() + Duration::from_secs(1),
            network: rumqttc_wrapper_core::NetworkConfig::default(),
            mode: TransportMode::Base,
        }
    }
    fn response() -> rumqttc_transport_response_t {
        rumqttc_transport_response_t {
            struct_size: struct_size::<rumqttc_transport_response_t>(),
            result: 0,
            stream: ptr::null(),
            network_handling: 0,
            reserved_flags: 0,
            bytes: rumqttc_bytes_view_t {
                data: ptr::null(),
                len: 0,
            },
            count: 0,
            reserved: [0; 2],
        }
    }
    fn new_stream(
        token: *mut rumqttc_callback_completion,
        host: &Arc<Host>,
    ) -> *mut rumqttc_transport_stream {
        let table = rumqttc_transport_stream_vtable_t {
            struct_size: struct_size::<rumqttc_transport_stream_vtable_t>(),
            mode: 1,
            perform: Some(deferred_io),
            cancel: Some(cancelled),
            destroy: Some(stream_destroyed),
            reserved: [0; 2],
        };
        let mut stream = ptr::null_mut();
        assert_eq!(
            unsafe {
                rumqttc_transport_stream_new(
                    token,
                    &raw const table,
                    Box::into_raw(Box::new(host.clone())).cast(),
                    &raw mut stream,
                    ptr::null_mut(),
                )
            },
            OK
        );
        stream
    }
    async fn connected(connector: &Connector, host: &Arc<Host>) -> TransportConnection {
        let future = connector.connect(request(1));
        {
            let token = token(host);
            let stream = new_stream(token, host);
            let mut reply = response();
            reply.stream = stream;
            reply.network_handling = 2;
            assert_eq!(
                unsafe { rumqttc_callback_transport_complete(token, &raw const reply) },
                OK
            );
            unsafe {
                rumqttc_transport_stream_destroy(stream);
                rumqttc_callback_completion_destroy(token);
            }
        }
        future.await.unwrap()
    }
    #[tokio::test]
    async fn cancellation_is_immediate_and_retained_cancelled_work_enforces_the_budget() {
        let host = Arc::new(Host::default());
        let (connector, registration) = registration(&host, 1);
        let future = connector.connect(request(1));
        let token = token(&host);
        drop(future);
        assert_eq!(host.cancellations.load(Ordering::SeqCst), 1);
        assert!(matches!(
            connector.connect(request(2)).await,
            Err(TransportFailure::ResourceLimit)
        ));
        assert_eq!(
            unsafe { rumqttc_callback_transport_complete(token, std::ptr::dangling()) },
            crate::error::INVALID_STATE
        );
        unsafe {
            rumqttc_transport_registration_destroy(registration);
        }
        drop(connector);
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 0);
        unsafe {
            rumqttc_callback_completion_destroy(token);
        }
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn abandoning_the_last_host_token_wakes_the_observer() {
        let host = Arc::new(Host::default());
        let (connector, registration) = registration(&host, 1);
        let future = connector.connect(request(1));
        unsafe {
            rumqttc_callback_completion_destroy(token(&host));
        }
        assert!(matches!(future.await, Err(TransportFailure::Abandoned)));
        unsafe {
            rumqttc_transport_registration_destroy(registration);
        }
        drop(connector);
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn cancelled_streams_cannot_be_attached_to_a_new_attempt() {
        let host = Arc::new(Host::default());
        let (connector, registration) = registration(&host, 16);
        let first = connector.connect(request(1));
        let old_token = token(&host);
        let old_stream = new_stream(old_token, &host);
        drop(first);
        let second = connector.connect(request(2));
        let next_token = token(&host);
        let mut reply = response();
        reply.stream = old_stream;
        reply.network_handling = 2;
        assert_eq!(
            unsafe { rumqttc_callback_transport_complete(next_token, &raw const reply) },
            crate::error::INVALID_STATE
        );
        let fresh = new_stream(next_token, &host);
        reply.stream = fresh;
        assert_eq!(
            unsafe { rumqttc_callback_transport_complete(next_token, &raw const reply) },
            OK
        );
        assert_eq!(
            unsafe { rumqttc_callback_transport_complete(next_token, std::ptr::dangling()) },
            crate::error::INVALID_STATE
        );
        drop(second.await.unwrap());
        unsafe {
            rumqttc_callback_completion_destroy(old_token);
            rumqttc_callback_completion_destroy(next_token);
            rumqttc_transport_stream_destroy(old_stream);
            rumqttc_transport_stream_destroy(fresh);
            rumqttc_transport_registration_destroy(registration);
        }
        drop(connector);
        assert_eq!(host.streams_destroyed.load(Ordering::SeqCst), 2);
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn retained_stream_work_owns_write_bytes_and_rejects_oversized_read_views() {
        let host = Arc::new(Host::default());
        let (connector, registration) = registration(&host, 16);
        let connection = connected(&connector, &host).await;
        let read = connection.io.read(4);
        let read_token = token(&host);
        let mut reply = response();
        reply.bytes.data = std::ptr::dangling();
        reply.bytes.len = 5;
        assert_eq!(
            unsafe { rumqttc_callback_transport_complete(read_token, &raw const reply) },
            OK
        );
        assert_eq!(
            read.await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        unsafe {
            rumqttc_callback_completion_destroy(read_token);
        }
        let input = Bytes::from(vec![1, 2, 3, 4]);
        let work = connection.io.write(input.clone());
        let write_token = token(&host);
        drop(input);
        let CallbackCompletion::Transport(operation) = (unsafe { &(*write_token).inner }) else {
            unreachable!()
        };
        assert_eq!(&operation.input[..], &[1, 2, 3, 4]);
        let mut reply = response();
        reply.count = 2;
        assert_eq!(
            unsafe { rumqttc_callback_transport_complete(write_token, &raw const reply) },
            OK
        );
        assert_eq!(work.await.unwrap(), 2);
        unsafe {
            rumqttc_callback_completion_destroy(write_token);
        }
        drop(connection);
        unsafe {
            rumqttc_transport_registration_destroy(registration);
        }
        drop(connector);
        assert_eq!(host.streams_destroyed.load(Ordering::SeqCst), 1);
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn a_successful_connect_must_apply_nondefault_network_settings() {
        let host = Arc::new(Host::default());
        let (connector, registration) = registration(&host, 16);
        let mut request = request(1);
        request.network.tcp_nodelay = true;
        let future = connector.connect(request);
        let token = token(&host);
        let stream = new_stream(token, &host);
        let mut reply = response();
        reply.stream = stream;
        reply.network_handling = 2;
        assert_eq!(
            unsafe { rumqttc_callback_transport_complete(token, &raw const reply) },
            OK
        );
        assert!(matches!(
            future.await,
            Err(TransportFailure::NetworkOptions)
        ));
        unsafe {
            rumqttc_callback_completion_destroy(token);
            rumqttc_transport_stream_destroy(stream);
            rumqttc_transport_registration_destroy(registration);
        }
        drop(connector);
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 1);
    }
}
