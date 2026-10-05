//! Deferred TLS registrations using the shared retained completion contract.
use super::super::{CallbackCompletion, rumqttc_callback_completion};
use super::{
    Arc, AtomicBool, ErrorHandle, IdentityConfig, Instant, MAX_TLS_SIGNATURE_BYTES, Ordering,
    Owner, TlsCallbackReason, VerifierConfig, boundary, bytes_from_view, c_void, catalog, checked,
    enabled, ptr, remaining, reply, rumqttc_bytes_view_t, rumqttc_error,
    rumqttc_tls_external_identity_t, rumqttc_tls_identity_registration,
    rumqttc_tls_identity_request_t, rumqttc_tls_signing_request_t,
    rumqttc_tls_verification_request_t, rumqttc_tls_verifier_registration, struct_size, view_bytes,
    view_string, write_optional,
};
use crate::error::{INVALID_ARGUMENT, INVALID_STATE, OK};
use rumqttc_wrapper_core::{
    AsyncTlsExternalIdentityConfig, AsyncTlsIdentityProvider, AsyncTlsIdentityRequest,
    AsyncTlsSigningRequest, AsyncTlsVerificationRequest, AsyncTlsVerifier, AsyncTlsVerifierConfig,
    TlsCallbackFuture, TlsCallbackStage,
};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, AtomicUsize},
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_tls_async_verifier_vtable_t {
    pub struct_size: u32,
    pub max_retained_operations: u32,
    pub verify: Option<
        unsafe extern "C" fn(
            *mut c_void,
            u64,
            *const rumqttc_tls_verification_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub cancel: Option<unsafe extern "C" fn(*mut c_void, u64)>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_tls_async_identity_vtable_t {
    pub struct_size: u32,
    pub max_retained_operations: u32,
    pub select: Option<
        unsafe extern "C" fn(
            *mut c_void,
            u64,
            *const rumqttc_tls_identity_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub sign: Option<
        unsafe extern "C" fn(
            *mut c_void,
            u64,
            *const rumqttc_tls_signing_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub cancel: Option<unsafe extern "C" fn(*mut c_void, u64)>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}
struct AsyncOwner {
    owner: Owner,
    cancel: unsafe extern "C" fn(*mut c_void, u64),
    limit: usize,
    outstanding: AtomicUsize,
    next_id: AtomicU64,
}
impl AsyncOwner {
    fn new(
        data: *mut c_void,
        cancel: unsafe extern "C" fn(*mut c_void, u64),
        destroy: unsafe extern "C" fn(*mut c_void),
        limit: u32,
    ) -> Self {
        Self {
            owner: Owner {
                data: data as usize,
                destroy,
                armed: AtomicBool::new(false),
            },
            cancel,
            limit: limit as usize,
            outstanding: AtomicUsize::new(0),
            next_id: AtomicU64::new(0),
        }
    }
}
enum Value {
    Verified,
    Selected(Option<usize>),
    Signed(Vec<u8>),
}
type Response = Result<Value, TlsCallbackReason>;
enum State {
    Pending(tokio::sync::oneshot::Sender<Response>),
    Completed,
    Cancelled,
}
#[allow(
    clippy::redundant_pub_crate,
    reason = "Keep the shared operation out of public C Rust reexports"
)]
pub(crate) struct TlsOperation {
    owner: Arc<AsyncOwner>,
    state: Mutex<State>,
    hosts: AtomicUsize,
    id: u64,
    stage: TlsCallbackStage,
    deadline: Option<Instant>,
    identity_count: usize,
}
impl Drop for TlsOperation {
    fn drop(&mut self) {
        self.owner.outstanding.fetch_sub(1, Ordering::AcqRel);
    }
}
impl TlsOperation {
    pub(crate) fn retain_host(&self) {
        self.hosts.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn release_host(&self) {
        if self.hosts.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.finish(|| Ok(Err(TlsCallbackReason::Abandoned)));
        }
    }
    fn finish(&self, make: impl FnOnce() -> Result<Response, u32>) -> u32 {
        let (sender, result) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(&*state, State::Pending(sender) if !sender.is_closed())
                || self.deadline.is_some_and(|d| Instant::now() >= d)
            {
                return INVALID_STATE;
            }
            let result = match make() {
                Ok(v) => v,
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
            INVALID_STATE
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
            unsafe { (self.owner.cancel)(self.owner.owner.data as *mut c_void, self.id) };
        }
    }
}
struct Cancel(Arc<TlsOperation>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
#[allow(deprecated, reason = "fetch_update is available at the Rust 1.88 MSRV")]
fn operation(
    owner: Arc<AsyncOwner>,
    stage: TlsCallbackStage,
    deadline: Option<Instant>,
    identity_count: usize,
) -> Result<(Arc<TlsOperation>, TlsCallbackFuture<Value>), TlsCallbackReason> {
    let id = owner
        .next_id
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| TlsCallbackReason::ResourceLimit)?
        + 1;
    owner
        .outstanding
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < owner.limit).then_some(n + 1)
        })
        .map_err(|_| TlsCallbackReason::ResourceLimit)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let inner = Arc::new(TlsOperation {
        owner,
        state: Mutex::new(State::Pending(sender)),
        hosts: AtomicUsize::new(1),
        id,
        stage,
        deadline,
        identity_count,
    });
    let cancel = Cancel(inner.clone()); // Also covers cancellation before the first poll.
    Ok((
        inner,
        Box::pin(async move {
            let _cancel = cancel;
            receiver.await.unwrap_or(Err(TlsCallbackReason::Abandoned))
        }),
    ))
}
struct Verifier {
    owner: Arc<AsyncOwner>,
    table: rumqttc_tls_async_verifier_vtable_t,
}
struct Identity {
    owner: Arc<AsyncOwner>,
    table: rumqttc_tls_async_identity_vtable_t,
    count: usize,
}
impl AsyncTlsVerifier for Verifier {
    fn verify(&self, r: AsyncTlsVerificationRequest) -> TlsCallbackFuture<()> {
        let (inner, future) = match operation(
            self.owner.clone(),
            TlsCallbackStage::Verification,
            r.deadline,
            0,
        ) {
            Ok(v) => v,
            Err(e) => return Box::pin(std::future::ready(Err(e))),
        };
        let certificates: Vec<_> = r.certificates.iter().map(|b| view_bytes(b)).collect();
        let request = rumqttc_tls_verification_request_t {
            struct_size: struct_size::<rumqttc_tls_verification_request_t>(),
            layer: r.layer as u32,
            server_name: view_string(&r.server_name),
            certificates: certificates.as_ptr(),
            certificate_count: certificates.len(),
            ocsp_response: view_bytes(&r.ocsp_response),
            unix_time: r.unix_time,
            remaining_ns: remaining(r.deadline),
            reserved: [0; 2],
        };
        let mut token = rumqttc_callback_completion {
            inner: CallbackCompletion::Tls(inner.clone()),
        };
        unsafe {
            self.table.verify.expect("validated verifier")(
                self.owner.owner.data as *mut c_void,
                inner.id,
                &raw const request,
                &raw mut token,
            );
        };
        Box::pin(async move {
            match future.await? {
                Value::Verified => Ok(()),
                _ => Err(TlsCallbackReason::InvalidResponse),
            }
        })
    }
}
impl AsyncTlsIdentityProvider for Identity {
    fn select(&self, r: AsyncTlsIdentityRequest) -> TlsCallbackFuture<Option<usize>> {
        let (inner, future) = match operation(
            self.owner.clone(),
            TlsCallbackStage::IdentitySelection,
            r.deadline,
            self.count,
        ) {
            Ok(v) => v,
            Err(e) => return Box::pin(std::future::ready(Err(e))),
        };
        let issuers: Vec<_> = r.issuer_hints.iter().map(|b| view_bytes(b)).collect();
        let request = rumqttc_tls_identity_request_t {
            struct_size: struct_size::<rumqttc_tls_identity_request_t>(),
            layer: r.layer as u32,
            server_name: view_string(&r.server_name),
            issuer_hints: issuers.as_ptr(),
            issuer_hint_count: issuers.len(),
            signature_schemes: r.signature_schemes.as_ptr(),
            signature_scheme_count: r.signature_schemes.len(),
            remaining_ns: remaining(r.deadline),
            reserved: [0; 2],
        };
        let mut token = rumqttc_callback_completion {
            inner: CallbackCompletion::Tls(inner.clone()),
        };
        unsafe {
            self.table.select.expect("validated selector")(
                self.owner.owner.data as *mut c_void,
                inner.id,
                &raw const request,
                &raw mut token,
            );
        };
        Box::pin(async move {
            match future.await? {
                Value::Selected(v) => Ok(v),
                _ => Err(TlsCallbackReason::InvalidResponse),
            }
        })
    }
    fn sign(&self, r: AsyncTlsSigningRequest) -> TlsCallbackFuture<Vec<u8>> {
        let (inner, future) = match operation(
            self.owner.clone(),
            TlsCallbackStage::Signing,
            r.deadline,
            self.count,
        ) {
            Ok(v) => v,
            Err(e) => return Box::pin(std::future::ready(Err(e))),
        };
        let request = rumqttc_tls_signing_request_t {
            struct_size: struct_size::<rumqttc_tls_signing_request_t>(),
            layer: r.layer as u32,
            server_name: view_string(&r.server_name),
            identity_index: r.identity_index,
            key_id: view_bytes(&r.key_id),
            signature_scheme: u32::from(r.signature_scheme),
            message: view_bytes(&r.message),
            remaining_ns: remaining(r.deadline),
            reserved: [0; 2],
        };
        let mut token = rumqttc_callback_completion {
            inner: CallbackCompletion::Tls(inner.clone()),
        };
        unsafe {
            self.table.sign.expect("validated signer")(
                self.owner.owner.data as *mut c_void,
                inner.id,
                &raw const request,
                &raw mut token,
            );
        };
        Box::pin(async move {
            match future.await? {
                Value::Signed(v) => Ok(v),
                _ => Err(TlsCallbackReason::InvalidResponse),
            }
        })
    }
}
fn valid_limit(limit: u32) -> bool {
    (1..=65536).contains(&limit)
}
/// # Safety
/// Pointers and records must obey the ownership and size contract in `rumqttc.h`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_verifier_registration_new_async(
    table: *const rumqttc_tls_async_verifier_vtable_t,
    data: *mut c_void,
    out: *mut *mut rumqttc_tls_verifier_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("TLS registration output is NULL"));
        }
        let table = *unsafe { checked(table) }?;
        if table.verify.is_none()
            || table.cancel.is_none()
            || table.destroy.is_none()
            || table.reserved != [0; 2]
            || !valid_limit(table.max_retained_operations)
        {
            return Err(ErrorHandle::argument(
                "invalid deferred TLS verifier vtable",
            ));
        }
        let owner = Arc::new(AsyncOwner::new(
            data,
            table
                .cancel
                .ok_or_else(|| ErrorHandle::argument("TLS cancellation callback is NULL"))?,
            table
                .destroy
                .ok_or_else(|| ErrorHandle::argument("TLS destructor is NULL"))?,
            table.max_retained_operations,
        ));
        let config = AsyncTlsVerifierConfig(Arc::new(Verifier {
            owner: owner.clone(),
            table,
        }));
        let registration = Box::new(rumqttc_tls_verifier_registration {
            config: VerifierConfig::Async(config),
        });
        owner.owner.armed.store(true, Ordering::Release);
        unsafe {
            *out = Box::into_raw(registration);
        }
        Ok(())
    })
}
/// # Safety
/// Pointers and records must obey the ownership and size contract in `rumqttc.h`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_identity_registration_new_async(
    table: *const rumqttc_tls_async_identity_vtable_t,
    data: *mut c_void,
    identities: *const rumqttc_tls_external_identity_t,
    count: usize,
    out: *mut *mut rumqttc_tls_identity_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("TLS registration output is NULL"));
        }
        let table = *unsafe { checked(table) }?;
        if table.select.is_none()
            || table.sign.is_none()
            || table.cancel.is_none()
            || table.destroy.is_none()
            || table.reserved != [0; 2]
            || !valid_limit(table.max_retained_operations)
        {
            return Err(ErrorHandle::argument(
                "invalid deferred TLS identity vtable",
            ));
        }
        let catalog = unsafe { catalog(identities, count) }?;
        let owner = Arc::new(AsyncOwner::new(
            data,
            table
                .cancel
                .ok_or_else(|| ErrorHandle::argument("TLS cancellation callback is NULL"))?,
            table
                .destroy
                .ok_or_else(|| ErrorHandle::argument("TLS destructor is NULL"))?,
            table.max_retained_operations,
        ));
        let config = AsyncTlsExternalIdentityConfig::new(
            catalog,
            Arc::new(Identity {
                owner: owner.clone(),
                table,
                count,
            }),
        )
        .map_err(|e| ErrorHandle::from_core(&e, None))?;
        let registration = Box::new(rumqttc_tls_identity_registration {
            config: IdentityConfig::Async(config),
        });
        owner.owner.armed.store(true, Ordering::Release);
        unsafe {
            *out = Box::into_raw(registration);
        }
        Ok(())
    })
}
fn response(reason: u32, value: impl FnOnce() -> Result<Value, u32>) -> Result<Response, u32> {
    match reason {
        0 => value().map(Ok),
        1 | 2 | 7 | 8 => Ok(Err(reply(reason).unwrap_err())),
        _ => Err(INVALID_ARGUMENT),
    }
}
fn complete(
    token: *mut rumqttc_callback_completion,
    stage: TlsCallbackStage,
    value: impl FnOnce(&TlsOperation) -> Result<Response, u32>,
) -> u32 {
    if token.is_null() {
        return INVALID_ARGUMENT;
    }
    let CallbackCompletion::Tls(op) = (unsafe { &(*token).inner }) else {
        return INVALID_ARGUMENT;
    };
    op.finish(|| {
        if op.stage != stage {
            return Err(INVALID_ARGUMENT);
        }
        value(op)
    })
}
/// # Safety
/// `token` must be a live borrowed or retained callback completion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_tls_verify_complete(
    token: *mut rumqttc_callback_completion,
    reason: u32,
) -> u32 {
    super::super::boundary(ptr::null_mut(), ptr::null_mut(), || {
        let status = complete(token, TlsCallbackStage::Verification, |_| {
            response(reason, || Ok(Value::Verified))
        });
        completion_status(status)
    })
}
/// # Safety
/// `token` must be a live borrowed or retained callback completion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_tls_select_complete(
    token: *mut rumqttc_callback_completion,
    reason: u32,
    index: usize,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        completion_status(complete(token, TlsCallbackStage::IdentitySelection, |op| {
            response(reason, || {
                if index != usize::MAX && index >= op.identity_count {
                    return Err(INVALID_ARGUMENT);
                }
                Ok(Value::Selected((index != usize::MAX).then_some(index)))
            })
        }))
    })
}
/// # Safety
/// `token` must be live. Signature views must be valid when accepting success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_tls_sign_complete(
    token: *mut rumqttc_callback_completion,
    reason: u32,
    signature: rumqttc_bytes_view_t,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        completion_status(complete(token, TlsCallbackStage::Signing, |_| {
            response(reason, || {
                if signature.len == 0 || signature.len > MAX_TLS_SIGNATURE_BYTES {
                    return Err(INVALID_ARGUMENT);
                }
                let bytes = unsafe { bytes_from_view(signature) }.map_err(|_| INVALID_ARGUMENT)?;
                Ok(Value::Signed(bytes.to_vec()))
            })
        }))
    })
}
fn completion_status(status: u32) -> Result<(), ErrorHandle> {
    match status {
        OK => Ok(()),
        INVALID_STATE => Err(ErrorHandle::state("TLS operation is no longer pending")),
        _ => Err(ErrorHandle::argument("invalid TLS completion response")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{
        rumqttc_callback_completion_destroy, rumqttc_callback_completion_retain,
    };
    use super::*;
    #[derive(Default)]
    struct Counts {
        cancel: AtomicUsize,
        destroy: AtomicUsize,
    }
    unsafe extern "C" fn cancel(data: *mut c_void, _: u64) {
        unsafe { &*data.cast::<Counts>() }
            .cancel
            .fetch_add(1, Ordering::SeqCst);
    }
    unsafe extern "C" fn destroy(data: *mut c_void) {
        unsafe { &*data.cast::<Counts>() }
            .destroy
            .fetch_add(1, Ordering::SeqCst);
    }
    fn owner(counts: &Counts, limit: u32) -> Arc<AsyncOwner> {
        let owner = Arc::new(AsyncOwner::new(
            std::ptr::from_ref(counts).cast_mut().cast(),
            cancel,
            destroy,
            limit,
        ));
        owner.owner.armed.store(true, Ordering::Release);
        owner
    }
    fn token(op: Arc<TlsOperation>) -> rumqttc_callback_completion {
        rumqttc_callback_completion {
            inner: CallbackCompletion::Tls(op),
        }
    }
    fn wait<T>(future: TlsCallbackFuture<T>) -> Result<T, TlsCallbackReason> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }
    #[test]
    fn response_validation_is_transactional_and_signatures_are_copied() {
        let counts = Counts::default();
        let owner = owner(&counts, 4);
        let (op, future) = operation(owner.clone(), TlsCallbackStage::Signing, None, 1).unwrap();
        let mut token = token(op);
        unsafe {
            assert_eq!(
                rumqttc_callback_tls_verify_complete(&raw mut token, 0),
                INVALID_ARGUMENT
            );
            assert_eq!(
                rumqttc_callback_tls_sign_complete(&raw mut token, 0, view_bytes(&[])),
                INVALID_ARGUMENT
            );
            assert_eq!(
                rumqttc_callback_tls_sign_complete(&raw mut token, 99, view_bytes(&[1])),
                INVALID_ARGUMENT
            );
            let mut bytes = vec![1, 2, 3];
            assert_eq!(
                rumqttc_callback_tls_sign_complete(&raw mut token, 0, view_bytes(&bytes)),
                OK
            );
            bytes.fill(9);
            assert_eq!(
                rumqttc_callback_tls_sign_complete(
                    &raw mut token,
                    0,
                    rumqttc_bytes_view_t {
                        data: ptr::dangling(),
                        len: 3
                    }
                ),
                INVALID_STATE
            );
        }
        let Value::Signed(bytes) = wait(future).unwrap() else {
            panic!()
        };
        assert_eq!(bytes, vec![1, 2, 3]);
        drop(token);
        drop(owner);
        assert_eq!(counts.destroy.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn pending_tokens_cancel_once_and_reject_late_buffers_without_reading_them() {
        let counts = Counts::default();
        let owner = owner(&counts, 1);
        let (op, future) = operation(owner.clone(), TlsCallbackStage::Signing, None, 1).unwrap();
        let mut borrowed = token(op);
        let mut retained = ptr::null_mut();
        assert_eq!(
            unsafe { rumqttc_callback_completion_retain(&raw mut borrowed, &raw mut retained) },
            OK
        );
        drop(borrowed);
        drop(future);
        drop(owner);
        assert_eq!(counts.cancel.load(Ordering::SeqCst), 1);
        assert_eq!(counts.destroy.load(Ordering::SeqCst), 0);
        assert_eq!(
            unsafe {
                rumqttc_callback_tls_sign_complete(
                    retained,
                    0,
                    rumqttc_bytes_view_t {
                        data: ptr::dangling(),
                        len: 17,
                    },
                )
            },
            INVALID_STATE
        );
        unsafe {
            rumqttc_callback_completion_destroy(retained);
        }
        assert_eq!(counts.cancel.load(Ordering::SeqCst), 1);
        assert_eq!(counts.destroy.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn releasing_last_unanswered_token_wakes_future_with_terminal_abandonment() {
        let counts = Counts::default();
        let owner = owner(&counts, 1);
        let (op, future) =
            operation(owner.clone(), TlsCallbackStage::Verification, None, 0).unwrap();
        drop(token(op));
        assert!(matches!(wait(future), Err(TlsCallbackReason::Abandoned)));
        assert_eq!(counts.cancel.load(Ordering::SeqCst), 0);
        drop(owner);
        assert_eq!(counts.destroy.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn retained_finished_operations_bound_resources_and_selection_can_decline() {
        let counts = Counts::default();
        let owner = owner(&counts, 1);
        let (op, future) =
            operation(owner.clone(), TlsCallbackStage::IdentitySelection, None, 1).unwrap();
        let mut token = token(op);
        assert_eq!(
            unsafe { rumqttc_callback_tls_select_complete(&raw mut token, 0, 1) },
            INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { rumqttc_callback_tls_select_complete(&raw mut token, 0, usize::MAX) },
            OK
        );
        assert!(matches!(wait(future), Ok(Value::Selected(None))));
        assert!(matches!(
            operation(owner.clone(), TlsCallbackStage::Signing, None, 1),
            Err(TlsCallbackReason::ResourceLimit)
        ));
        drop(token);
        let (op, future) = operation(owner.clone(), TlsCallbackStage::Signing, None, 1).unwrap();
        assert_eq!(op.id, 3);
        drop(token_for_test(op));
        drop(future);
        owner.next_id.store(u64::MAX, Ordering::SeqCst);
        assert!(matches!(
            operation(owner.clone(), TlsCallbackStage::Signing, None, 1),
            Err(TlsCallbackReason::ResourceLimit)
        ));
        drop(owner);
        assert_eq!(counts.destroy.load(Ordering::SeqCst), 1);
    }
    fn token_for_test(op: Arc<TlsOperation>) -> rumqttc_callback_completion {
        token(op)
    }
    #[test]
    fn expired_completion_rejects_response_and_still_notifies_cancellation() {
        let counts = Counts::default();
        let owner = owner(&counts, 1);
        let (op, future) = operation(
            owner.clone(),
            TlsCallbackStage::Verification,
            Some(Instant::now()),
            0,
        )
        .unwrap();
        let mut token = token(op);
        assert_eq!(
            unsafe { rumqttc_callback_tls_verify_complete(&raw mut token, 0) },
            INVALID_STATE
        );
        drop(future);
        drop(token);
        drop(owner);
        assert_eq!(counts.cancel.load(Ordering::SeqCst), 1);
        assert_eq!(counts.destroy.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn concurrent_completion_and_cancellation_consume_the_operation_once() {
        for _ in 0..64 {
            let counts = Counts::default();
            let owner = owner(&counts, 1);
            let (op, future) =
                operation(owner.clone(), TlsCallbackStage::Verification, None, 0).unwrap();
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                let task = scope.spawn(|| {
                    barrier.wait();
                    op.finish(|| Ok(Ok(Value::Verified)))
                });
                barrier.wait();
                drop(future);
                let status = task.join().unwrap();
                assert!(matches!(status, OK | INVALID_STATE));
            });
            let cancelled = matches!(*op.state.lock().unwrap(), State::Cancelled);
            assert_eq!(counts.cancel.load(Ordering::SeqCst), usize::from(cancelled));
            assert_eq!(
                op.finish(|| panic!("late response was inspected")),
                INVALID_STATE
            );
            drop(token(op));
            drop(owner);
            assert_eq!(counts.destroy.load(Ordering::SeqCst), 1);
        }
    }
    #[test]
    fn failed_async_constructors_leave_host_ownership_with_the_caller() {
        unsafe extern "C" fn verify(
            _: *mut c_void,
            _: u64,
            _: *const rumqttc_tls_verification_request_t,
            _: *mut rumqttc_callback_completion,
        ) {
        }
        unsafe extern "C" fn select(
            _: *mut c_void,
            _: u64,
            _: *const rumqttc_tls_identity_request_t,
            _: *mut rumqttc_callback_completion,
        ) {
        }
        unsafe extern "C" fn sign(
            _: *mut c_void,
            _: u64,
            _: *const rumqttc_tls_signing_request_t,
            _: *mut rumqttc_callback_completion,
        ) {
        }
        let counts = Counts::default();
        let data = std::ptr::from_ref(&counts).cast_mut().cast();
        let verifier = rumqttc_tls_async_verifier_vtable_t {
            struct_size: struct_size::<rumqttc_tls_async_verifier_vtable_t>(),
            max_retained_operations: 0,
            verify: Some(verify),
            cancel: Some(cancel),
            destroy: Some(destroy),
            reserved: [0; 2],
        };
        let identity = rumqttc_tls_async_identity_vtable_t {
            struct_size: struct_size::<rumqttc_tls_async_identity_vtable_t>(),
            max_retained_operations: 64,
            select: Some(select),
            sign: Some(sign),
            cancel: Some(cancel),
            destroy: Some(destroy),
            reserved: [0; 2],
        };
        let mut verifier_out = ptr::dangling_mut();
        let mut identity_out = ptr::dangling_mut();
        unsafe {
            assert_ne!(
                rumqttc_tls_verifier_registration_new_async(
                    &raw const verifier,
                    data,
                    &raw mut verifier_out,
                    ptr::null_mut()
                ),
                OK
            );
            assert_ne!(
                rumqttc_tls_identity_registration_new_async(
                    &raw const identity,
                    data,
                    ptr::null(),
                    0,
                    &raw mut identity_out,
                    ptr::null_mut()
                ),
                OK
            );
        }
        assert!(verifier_out.is_null());
        assert!(identity_out.is_null());
        assert_eq!(counts.cancel.load(Ordering::SeqCst), 0);
        assert_eq!(counts.destroy.load(Ordering::SeqCst), 0);
    }
}
