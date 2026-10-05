//! Owned TLS registrations. Callback views never escape a call.
use super::{
    CAP_RUSTLS, boundary, bytes_from_view, destroy_box, error_ref, ptr, rumqttc_bytes_view_t,
    rumqttc_error, rumqttc_library_capabilities, rumqttc_string_view_t, rumqttc_tls_profile,
    rumqttc_tls_profile_options_t, struct_size, view_bytes, view_string, write_optional,
};
use crate::error::ErrorHandle;
use rumqttc_wrapper_core::{
    MAX_TLS_CIPHER_SUITES, MAX_TLS_IDENTITIES, MAX_TLS_KEY_ID_BYTES, MAX_TLS_METADATA_BYTES,
    MAX_TLS_SIGNATURE_BYTES, TlsAdvancedCapabilities, TlsBackend, TlsCallbackReason,
    TlsClientIdentity, TlsExternalIdentity, TlsExternalIdentityConfig, TlsIdentityProvider,
    TlsIdentityRequest, TlsResumptionPolicy, TlsSigningRequest, TlsSniPolicy,
    TlsVerificationRequest, TlsVerifier, TlsVerifierConfig,
};
use std::{
    ffi::c_void,
    slice,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[path = "tls_deferred.rs"]
mod deferred;
pub use deferred::*;
#[derive(Clone)]
enum VerifierConfig {
    Sync(TlsVerifierConfig),
    Async(rumqttc_wrapper_core::AsyncTlsVerifierConfig),
}
#[derive(Clone)]
enum IdentityConfig {
    Sync(TlsExternalIdentityConfig),
    Async(rumqttc_wrapper_core::AsyncTlsExternalIdentityConfig),
}

#[repr(C)]
pub struct rumqttc_tls_advanced_capabilities_t {
    pub struct_size: u32,
    pub sni_policy_mask: u32,
    pub resumption_policy_mask: u32,
    pub feature_mask: u32,
    pub max_signature_bytes: u32,
    pub reserved: [u64; 2],
}
#[repr(C)]
pub struct rumqttc_tls_profile_extensions_t {
    pub struct_size: u32,
    pub sni_policy: u32,
    pub resumption_policy: u32,
    pub cipher_suites: *const u16,
    pub cipher_suite_count: usize,
    pub verifier: *const rumqttc_tls_verifier_registration,
    pub external_identity: *const rumqttc_tls_identity_registration,
    pub reserved: [u64; 2],
}
#[repr(C)]
pub struct rumqttc_tls_verification_request_t {
    pub struct_size: u32,
    pub layer: u32,
    pub server_name: rumqttc_string_view_t,
    pub certificates: *const rumqttc_bytes_view_t,
    pub certificate_count: usize,
    pub ocsp_response: rumqttc_bytes_view_t,
    pub unix_time: u64,
    /// Zero means no deadline; otherwise nanoseconds remaining, rounded up to one.
    pub remaining_ns: u64,
    pub reserved: [u64; 2],
}
#[repr(C)]
pub struct rumqttc_tls_identity_request_t {
    pub struct_size: u32,
    pub layer: u32,
    pub server_name: rumqttc_string_view_t,
    pub issuer_hints: *const rumqttc_bytes_view_t,
    pub issuer_hint_count: usize,
    pub signature_schemes: *const u16,
    pub signature_scheme_count: usize,
    pub remaining_ns: u64,
    pub reserved: [u64; 2],
}
#[repr(C)]
pub struct rumqttc_tls_signing_request_t {
    pub struct_size: u32,
    pub layer: u32,
    pub server_name: rumqttc_string_view_t,
    pub identity_index: usize,
    pub key_id: rumqttc_bytes_view_t,
    pub signature_scheme: u32,
    pub message: rumqttc_bytes_view_t,
    pub remaining_ns: u64,
    pub reserved: [u64; 2],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_tls_verifier_vtable_t {
    pub struct_size: u32,
    pub verify:
        Option<unsafe extern "C" fn(*mut c_void, *const rumqttc_tls_verification_request_t) -> u32>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_tls_identity_vtable_t {
    pub struct_size: u32,
    pub select: Option<
        unsafe extern "C" fn(*mut c_void, *const rumqttc_tls_identity_request_t, *mut usize) -> u32,
    >,
    pub sign: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_tls_signing_request_t,
            *mut u8,
            usize,
            *mut usize,
        ) -> u32,
    >,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}
#[repr(C)]
pub struct rumqttc_tls_external_identity_t {
    pub struct_size: u32,
    pub certificate_pem: rumqttc_bytes_view_t,
    pub key_id: rumqttc_bytes_view_t,
    pub signature_schemes: *const u16,
    pub signature_scheme_count: usize,
    pub reserved: [u64; 2],
}
pub struct rumqttc_tls_verifier_registration {
    config: VerifierConfig,
}
pub struct rumqttc_tls_identity_registration {
    config: IdentityConfig,
}
struct Owner {
    data: usize,
    destroy: unsafe extern "C" fn(*mut c_void),
    armed: AtomicBool,
}
// Only the host uses the pointer. The ABI requires callbacks and destruction to be thread-safe.
impl Drop for Owner {
    fn drop(&mut self) {
        if self.armed.load(Ordering::Acquire) {
            unsafe {
                (self.destroy)(self.data as *mut c_void);
            }
        }
    }
}
struct Verifier {
    owner: Owner,
    vtable: rumqttc_tls_verifier_vtable_t,
}
struct Identity {
    owner: Owner,
    vtable: rumqttc_tls_identity_vtable_t,
}
fn remaining(deadline: Option<Instant>) -> u64 {
    deadline.map_or(0, |d| {
        u64::try_from(d.saturating_duration_since(Instant::now()).as_nanos())
            .unwrap_or(u64::MAX)
            .max(1)
    })
}
const fn reply(value: u32) -> std::result::Result<(), TlsCallbackReason> {
    match value {
        0 => Ok(()),
        1 => Err(TlsCallbackReason::Rejected),
        2 => Err(TlsCallbackReason::Failed),
        7 => Err(TlsCallbackReason::Timeout),
        8 => Err(TlsCallbackReason::Transient),
        _ => Err(TlsCallbackReason::InvalidResponse),
    }
}
impl TlsVerifier for Verifier {
    fn verify(
        &self,
        request: &TlsVerificationRequest<'_>,
    ) -> std::result::Result<(), TlsCallbackReason> {
        let certificates: Vec<_> = request
            .certificates
            .iter()
            .map(|cert| view_bytes(cert))
            .collect();
        let context = rumqttc_tls_verification_request_t {
            struct_size: struct_size::<rumqttc_tls_verification_request_t>(),
            layer: request.layer as u32,
            server_name: view_string(request.server_name),
            certificates: certificates.as_ptr(),
            certificate_count: certificates.len(),
            ocsp_response: view_bytes(request.ocsp_response),
            unix_time: request.unix_time,
            remaining_ns: remaining(request.deadline),
            reserved: [0; 2],
        };
        reply(unsafe {
            self.vtable.verify.expect("validated callback")(
                self.owner.data as *mut c_void,
                &raw const context,
            )
        })
    }
}
impl TlsIdentityProvider for Identity {
    fn select(
        &self,
        request: &TlsIdentityRequest<'_>,
    ) -> std::result::Result<Option<usize>, TlsCallbackReason> {
        let issuers: Vec<_> = request
            .issuer_hints
            .iter()
            .map(|hint| view_bytes(hint))
            .collect();
        let context = rumqttc_tls_identity_request_t {
            struct_size: struct_size::<rumqttc_tls_identity_request_t>(),
            layer: request.layer as u32,
            server_name: view_string(request.server_name),
            issuer_hints: issuers.as_ptr(),
            issuer_hint_count: issuers.len(),
            signature_schemes: request.signature_schemes.as_ptr(),
            signature_scheme_count: request.signature_schemes.len(),
            remaining_ns: remaining(request.deadline),
            reserved: [0; 2],
        };
        let mut index = usize::MAX;
        reply(unsafe {
            self.vtable.select.expect("validated callback")(
                self.owner.data as *mut c_void,
                &raw const context,
                &raw mut index,
            )
        })?;
        Ok((index != usize::MAX).then_some(index))
    }
    fn sign(
        &self,
        request: &TlsSigningRequest<'_>,
    ) -> std::result::Result<Vec<u8>, TlsCallbackReason> {
        let context = rumqttc_tls_signing_request_t {
            struct_size: struct_size::<rumqttc_tls_signing_request_t>(),
            layer: request.layer as u32,
            server_name: view_string(request.server_name),
            identity_index: request.identity_index,
            key_id: view_bytes(request.key_id),
            signature_scheme: u32::from(request.signature_scheme),
            message: view_bytes(request.message),
            remaining_ns: remaining(request.deadline),
            reserved: [0; 2],
        };
        let mut signature = vec![0; MAX_TLS_SIGNATURE_BYTES];
        let mut written = 0;
        reply(unsafe {
            self.vtable.sign.expect("validated callback")(
                self.owner.data as *mut c_void,
                &raw const context,
                signature.as_mut_ptr(),
                signature.len(),
                &raw mut written,
            )
        })?;
        if written == 0 || written > signature.len() {
            return Err(TlsCallbackReason::InvalidResponse);
        }
        signature.truncate(written);
        Ok(signature)
    }
}
fn enabled() -> std::result::Result<(), ErrorHandle> {
    if rumqttc_library_capabilities() & CAP_RUSTLS == 0 {
        Err(ErrorHandle::plain(
            crate::error::CONFIG_ERROR,
            1,
            "TLS callbacks require Rustls",
        ))
    } else {
        Ok(())
    }
}
unsafe fn checked<'a, T>(value: *const T) -> std::result::Result<&'a T, ErrorHandle> {
    if value.is_null() || unsafe { value.cast::<u32>().read() } < struct_size::<T>() {
        return Err(ErrorHandle::argument("invalid TLS extension record"));
    }
    Ok(unsafe { &*value })
}
unsafe fn catalog(
    identities: *const rumqttc_tls_external_identity_t,
    identity_count: usize,
) -> Result<Vec<TlsExternalIdentity>, ErrorHandle> {
    if identities.is_null() || identity_count == 0 || identity_count > MAX_TLS_IDENTITIES {
        return Err(ErrorHandle::argument("invalid TLS identity count"));
    }
    let mut catalog = Vec::with_capacity(identity_count);
    let mut total = 0usize;
    for item in unsafe { slice::from_raw_parts(identities, identity_count) } {
        if item.struct_size < struct_size::<rumqttc_tls_external_identity_t>()
            || item.reserved != [0; 2]
            || item.signature_scheme_count == 0
            || item.signature_scheme_count > 10
            || item.signature_schemes.is_null()
            || item.key_id.len == 0
            || item.key_id.len > MAX_TLS_KEY_ID_BYTES
        {
            return Err(ErrorHandle::argument("invalid TLS identity descriptor"));
        }
        total = total
            .checked_add(item.certificate_pem.len)
            .and_then(|n| n.checked_add(item.key_id.len))
            .ok_or_else(|| ErrorHandle::argument("TLS identity catalog too large"))?;
        if total > MAX_TLS_METADATA_BYTES {
            return Err(ErrorHandle::argument("TLS identity catalog too large"));
        }
        catalog.push(TlsExternalIdentity {
            certificate_pem: bytes::Bytes::copy_from_slice(unsafe {
                bytes_from_view(item.certificate_pem)
            }?),
            key_id: bytes::Bytes::copy_from_slice(unsafe { bytes_from_view(item.key_id) }?),
            signature_schemes: unsafe {
                slice::from_raw_parts(item.signature_schemes, item.signature_scheme_count)
            }
            .to_vec(),
        });
    }
    Ok(catalog)
}
/// # Safety
/// Non-null pointers must satisfy the ownership, lifetime and alignment contract
/// in `rumqttc.h`; input records and output storage must cover their declared sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_verifier_registration_new(
    vtable: *const rumqttc_tls_verifier_vtable_t,
    data: *mut c_void,
    out: *mut *mut rumqttc_tls_verifier_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("TLS registration output is NULL"));
        }
        enabled()?;
        let vtable = *unsafe { checked(vtable) }?;
        if vtable.reserved != [0; 2] || vtable.verify.is_none() || vtable.destroy.is_none() {
            return Err(ErrorHandle::argument("invalid TLS verifier callbacks"));
        }
        let destroy = vtable
            .destroy
            .ok_or_else(|| ErrorHandle::argument("TLS destructor is NULL"))?;
        let verifier = Arc::new(Verifier {
            owner: Owner {
                data: data as usize,
                destroy,
                armed: AtomicBool::new(true),
            },
            vtable,
        });
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_tls_verifier_registration {
                config: VerifierConfig::Sync(TlsVerifierConfig(verifier)),
            }));
        }
        Ok(())
    })
}
/// # Safety
/// Non-null pointers must satisfy the ownership, lifetime and alignment contract
/// in `rumqttc.h`; input records and output storage must cover their declared sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_identity_registration_new(
    vtable: *const rumqttc_tls_identity_vtable_t,
    data: *mut c_void,
    identities: *const rumqttc_tls_external_identity_t,
    identity_count: usize,
    out: *mut *mut rumqttc_tls_identity_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("TLS registration output is NULL"));
        }
        enabled()?;
        let vtable = *unsafe { checked(vtable) }?;
        if vtable.reserved != [0; 2]
            || vtable.select.is_none()
            || vtable.sign.is_none()
            || vtable.destroy.is_none()
            || identities.is_null()
            || identity_count == 0
            || identity_count > MAX_TLS_IDENTITIES
        {
            return Err(ErrorHandle::argument("invalid TLS identity registration"));
        }
        let catalog = unsafe { catalog(identities, identity_count) }?;
        let destroy = vtable
            .destroy
            .ok_or_else(|| ErrorHandle::argument("TLS destructor is NULL"))?;
        let identity = Arc::new(Identity {
            owner: Owner {
                data: data as usize,
                destroy,
                armed: AtomicBool::new(false),
            },
            vtable,
        });
        let config = TlsExternalIdentityConfig::new(catalog, identity.clone())
            .map_err(|e| ErrorHandle::from_core(&e, None))?;
        let registration = Box::new(rumqttc_tls_identity_registration {
            config: IdentityConfig::Sync(config),
        });
        identity.owner.armed.store(true, Ordering::Release);
        unsafe {
            *out = Box::into_raw(registration);
        }
        Ok(())
    })
}
/// # Safety
/// Non-null pointers must satisfy the ownership, lifetime and alignment contract
/// in `rumqttc.h`; input records and output storage must cover their declared sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_verifier_registration_destroy(
    registration: *mut rumqttc_tls_verifier_registration,
) {
    unsafe {
        destroy_box(registration);
    }
}
/// # Safety
/// Non-null pointers must satisfy the ownership, lifetime and alignment contract
/// in `rumqttc.h`; input records and output storage must cover their declared sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_identity_registration_destroy(
    registration: *mut rumqttc_tls_identity_registration,
) {
    unsafe {
        destroy_box(registration);
    }
}
/// # Safety
/// Non-null pointers must satisfy the ownership, lifetime and alignment contract
/// in `rumqttc.h`; input records and output storage must cover their declared sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_profile_new_with_extensions(
    options: *const rumqttc_tls_profile_options_t,
    extensions: *const rumqttc_tls_profile_extensions_t,
    out: *mut *mut rumqttc_tls_profile,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("TLS profile output is NULL"));
        }
        let mut config = unsafe { super::tls::parse_profile_options(options) }?;
        let extensions = unsafe { checked(extensions) }?;
        if extensions.reserved != [0; 2]
            || extensions.cipher_suite_count > MAX_TLS_CIPHER_SUITES
            || (extensions.cipher_suite_count != 0 && extensions.cipher_suites.is_null())
        {
            return Err(ErrorHandle::argument("invalid TLS profile extensions"));
        }
        config.sni_policy = match extensions.sni_policy {
            0 => TlsSniPolicy::Default,
            1 => TlsSniPolicy::Enabled,
            2 => TlsSniPolicy::Disabled,
            _ => return Err(ErrorHandle::argument("unknown TLS SNI policy")),
        };
        config.resumption_policy = match extensions.resumption_policy {
            0 => TlsResumptionPolicy::Default,
            1 => TlsResumptionPolicy::Disabled,
            _ => return Err(ErrorHandle::argument("unknown TLS resumption policy")),
        };
        if extensions.cipher_suite_count != 0 {
            config.cipher_suites = unsafe {
                slice::from_raw_parts(extensions.cipher_suites, extensions.cipher_suite_count)
            }
            .to_vec();
        }
        if !extensions.verifier.is_null() {
            match &unsafe { &*extensions.verifier }.config {
                VerifierConfig::Sync(v) => config.verifier = Some(v.clone()),
                VerifierConfig::Async(v) => config.async_verifier = Some(v.clone()),
            }
        }
        if !extensions.external_identity.is_null() {
            if config.identity.is_some() {
                return Err(ErrorHandle::argument(
                    "static and external TLS identities are mutually exclusive",
                ));
            }
            config.identity = Some(match &unsafe { &*extensions.external_identity }.config {
                IdentityConfig::Sync(v) => TlsClientIdentity::External(v.clone()),
                IdentityConfig::Async(v) => TlsClientIdentity::ExternalAsync(v.clone()),
            });
        }
        config
            .validate()
            .map_err(|e| ErrorHandle::from_core(&e, None))?;
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_tls_profile { config }));
        }
        Ok(())
    })
}
/// # Safety
/// Non-null pointers must satisfy the ownership, lifetime and alignment contract
/// in `rumqttc.h`; input records and output storage must cover their declared sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_advanced_capabilities(
    backend: u32,
    out: *mut rumqttc_tls_advanced_capabilities_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let size = unsafe { checked(out) }?.struct_size;
        unsafe {
            *out = rumqttc_tls_advanced_capabilities_t {
                struct_size: size,
                sni_policy_mask: 0,
                resumption_policy_mask: 0,
                feature_mask: 0,
                max_signature_bytes: 0,
                reserved: [0; 2],
            };
        }
        let backend = match backend {
            0 => TlsBackend::Rustls,
            1 => TlsBackend::Native,
            _ => return Err(ErrorHandle::argument("unknown TLS backend")),
        };
        let enabled = match backend {
            TlsBackend::Rustls => rumqttc_library_capabilities() & super::CAP_RUSTLS != 0,
            TlsBackend::Native => rumqttc_library_capabilities() & super::CAP_NATIVE_TLS != 0,
        };
        let caps = if enabled {
            backend.advanced_capabilities()
        } else {
            TlsAdvancedCapabilities::default()
        };
        unsafe {
            (*out).sni_policy_mask = caps.sni_policies;
            (*out).resumption_policy_mask = caps.resumption_policies;
            (*out).feature_mask = u32::from(caps.cipher_selection)
                | (u32::from(caps.supplemental_verification) << 1)
                | (u32::from(caps.external_identities) << 2)
                | (u32::from(caps.deferred_verification) << 3)
                | (u32::from(caps.deferred_identities) << 4);
            (*out).max_signature_bytes = if caps.external_identities {
                u32::try_from(MAX_TLS_SIGNATURE_BYTES).map_err(|_| {
                    ErrorHandle::internal("TLS signature limit is not representable")
                })?
            } else {
                0
            };
        }
        Ok(())
    })
}
fn algorithms(
    backend: u32,
    signing: bool,
    out: *mut u16,
    capacity: usize,
    count: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(count, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if count.is_null() || (capacity != 0 && out.is_null()) {
            return Err(ErrorHandle::argument("invalid TLS algorithm output"));
        }
        enabled()?;
        if backend != 0 {
            return Err(ErrorHandle::argument(
                "TLS algorithm queries require Rustls",
            ));
        }
        let values = if signing {
            TlsBackend::Rustls.supported_signature_schemes()
        } else {
            TlsBackend::Rustls.supported_cipher_suites()
        }
        .map_err(|e| ErrorHandle::from_core(&e, None))?;
        unsafe {
            *count = values.len();
        }
        if capacity == 0 {
            return Ok(());
        }
        if capacity < values.len() {
            return Err(ErrorHandle::argument("TLS algorithm output too small"));
        }
        unsafe {
            ptr::copy_nonoverlapping(values.as_ptr(), out, values.len());
        }
        Ok(())
    })
}
/// # Safety
/// Non-null pointers must satisfy the ownership, lifetime and alignment contract
/// in `rumqttc.h`; input records and output storage must cover their declared sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_supported_cipher_suites(
    backend: u32,
    out: *mut u16,
    capacity: usize,
    count: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    algorithms(backend, false, out, capacity, count, error_out)
}
/// # Safety
/// Non-null pointers must satisfy the ownership, lifetime and alignment contract
/// in `rumqttc.h`; input records and output storage must cover their declared sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_supported_signature_schemes(
    backend: u32,
    out: *mut u16,
    capacity: usize,
    count: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    algorithms(backend, true, out, capacity, count, error_out)
}
/// # Safety
/// Non-null pointers must satisfy the ownership, lifetime and alignment contract
/// in `rumqttc.h`; input records and output storage must cover their declared sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_tls_callback_failure(
    error: *const rumqttc_error,
    present: *mut u8,
    stage: *mut u32,
    reason: *mut u32,
    layer: *mut u32,
) -> u32 {
    unsafe {
        write_optional(present, 0);
        write_optional(stage, 0);
        write_optional(reason, 0);
        write_optional(layer, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present.is_null() && stage.is_null() && reason.is_null() && layer.is_null() {
            return Err(ErrorHandle::argument("TLS failure output is NULL"));
        }
        if let Some(failure) = unsafe { error_ref(error) }?.failure_details().tls_callback {
            unsafe {
                write_optional(present, 1);
                write_optional(stage, failure.stage as u32);
                write_optional(reason, failure.reason as u32);
                write_optional(layer, failure.layer as u32);
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rumqttc_tls_profile_destroy;
    use std::sync::atomic::AtomicUsize;
    unsafe extern "C" fn verify(
        _: *mut c_void,
        _: *const rumqttc_tls_verification_request_t,
    ) -> u32 {
        panic!("validation must not call host callbacks")
    }
    unsafe extern "C" fn select(
        _: *mut c_void,
        _: *const rumqttc_tls_identity_request_t,
        _: *mut usize,
    ) -> u32 {
        panic!("validation must not call host callbacks")
    }
    unsafe extern "C" fn sign(
        _: *mut c_void,
        _: *const rumqttc_tls_signing_request_t,
        _: *mut u8,
        _: usize,
        _: *mut usize,
    ) -> u32 {
        panic!("validation must not call host callbacks")
    }
    unsafe extern "C" fn destroy(data: *mut c_void) {
        unsafe { &*data.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    }
    #[test]
    fn failed_registration_never_takes_host_ownership() {
        let destroyed = AtomicUsize::new(0);
        let vtable = rumqttc_tls_identity_vtable_t {
            struct_size: struct_size::<rumqttc_tls_identity_vtable_t>(),
            select: Some(select),
            sign: Some(sign),
            destroy: Some(destroy),
            reserved: [0; 2],
        };
        let scheme = 0x0403;
        let identity = rumqttc_tls_external_identity_t {
            struct_size: struct_size::<rumqttc_tls_external_identity_t>(),
            certificate_pem: view_bytes(b"invalid-private-certificate"),
            key_id: view_bytes(b"opaque-private-key"),
            signature_schemes: &raw const scheme,
            signature_scheme_count: 1,
            reserved: [0; 2],
        };
        let mut out = ptr::dangling_mut();
        let status = unsafe {
            rumqttc_tls_identity_registration_new(
                &raw const vtable,
                (&raw const destroyed).cast_mut().cast(),
                &raw const identity,
                1,
                &raw mut out,
                ptr::null_mut(),
            )
        };
        assert_ne!(status, 0);
        assert!(out.is_null());
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn retained_profiles_own_verification_registration_until_last_configuration_drops() {
        if enabled().is_err() {
            return;
        }
        let destroyed = AtomicUsize::new(0);
        let vtable = rumqttc_tls_verifier_vtable_t {
            struct_size: struct_size::<rumqttc_tls_verifier_vtable_t>(),
            verify: Some(verify),
            destroy: Some(destroy),
            reserved: [0; 2],
        };
        let mut registration = ptr::null_mut();
        assert_eq!(
            unsafe {
                rumqttc_tls_verifier_registration_new(
                    &raw const vtable,
                    (&raw const destroyed).cast_mut().cast(),
                    &raw mut registration,
                    ptr::null_mut(),
                )
            },
            0
        );
        let options = rumqttc_tls_profile_options_t {
            struct_size: struct_size::<rumqttc_tls_profile_options_t>(),
            version_policy: 0,
            tls: ptr::null(),
            pins: ptr::null(),
            pin_count: 0,
            reserved: [0; 2],
        };
        let mut extensions = rumqttc_tls_profile_extensions_t {
            struct_size: struct_size::<rumqttc_tls_profile_extensions_t>(),
            sni_policy: 99,
            resumption_policy: 0,
            cipher_suites: ptr::null(),
            cipher_suite_count: 0,
            verifier: registration,
            external_identity: ptr::null(),
            reserved: [0; 2],
        };
        let mut profile = ptr::dangling_mut();
        assert_ne!(
            unsafe {
                rumqttc_tls_profile_new_with_extensions(
                    &raw const options,
                    &raw const extensions,
                    &raw mut profile,
                    ptr::null_mut(),
                )
            },
            0
        );
        assert!(profile.is_null());
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
        extensions.sni_policy = 0;
        assert_eq!(
            unsafe {
                rumqttc_tls_profile_new_with_extensions(
                    &raw const options,
                    &raw const extensions,
                    &raw mut profile,
                    ptr::null_mut(),
                )
            },
            0
        );
        let retained = unsafe { &*profile }.config.clone();
        let retained_again = retained.clone();
        unsafe {
            rumqttc_tls_verifier_registration_destroy(registration);
            rumqttc_tls_profile_destroy(profile);
        }
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
        drop(retained);
        assert_eq!(destroyed.load(Ordering::SeqCst), 0);
        drop(retained_again);
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
    }
}
