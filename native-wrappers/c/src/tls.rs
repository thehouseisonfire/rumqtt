use std::{ptr, slice};

use crate::config::{set_transport_tls, set_transport_wss};
use crate::error::ErrorHandle;
use rumqttc_wrapper_core::{TlsBackend, TlsConfig};

use super::{
    CAP_NATIVE_TLS, CAP_RUSTLS, boundary, config_update, destroy_box, parse_proxy_options_with_tls,
    parse_tls_options, rumqttc_config, rumqttc_error, rumqttc_library_capabilities,
    rumqttc_proxy_options_t, rumqttc_string_view_t, rumqttc_tls_options_t, string_from_view,
    struct_size,
};
use rumqttc_wrapper_core::{
    MAX_TLS_PINS, ProtocolConfig, TlsPin, TlsPinTarget, TlsVersionPolicy, TransportConfig,
};

/// Immutable owned policy inputs. Configurations take independent owned copies.
pub struct rumqttc_tls_profile {
    pub(super) config: TlsConfig,
}

#[repr(C)]
pub struct rumqttc_tls_pin_t {
    pub struct_size: u32,
    pub target: u32,
    pub sha256: [u8; 32],
    pub reserved: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_tls_profile_options_t {
    pub struct_size: u32,
    pub version_policy: u32,
    pub tls: *const rumqttc_tls_options_t,
    pub pins: *const rumqttc_tls_pin_t,
    pub pin_count: usize,
    pub reserved: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_tls_backend_capabilities_t {
    pub struct_size: u32,
    pub version_policy_mask: u32,
    pub root_policy_mask: u32,
    pub pin_target_mask: u32,
    pub reserved: [u64; 2],
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_backend_capabilities(
    backend: u32,
    out: *mut rumqttc_tls_backend_capabilities_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("TLS capability output is NULL"));
        }
        let size = unsafe { ptr::addr_of!((*out).struct_size).read() };
        if size < struct_size::<rumqttc_tls_backend_capabilities_t>() {
            return Err(ErrorHandle::argument(
                "TLS capability output size is too small",
            ));
        }
        // Initialize before validating the backend; outputs remain zero on error.
        unsafe {
            *out = rumqttc_tls_backend_capabilities_t {
                struct_size: size,
                version_policy_mask: 0,
                root_policy_mask: 0,
                pin_target_mask: 0,
                reserved: [0; 2],
            }
        };
        let backend = match backend {
            0 => TlsBackend::Rustls,
            1 => TlsBackend::Native,
            _ => return Err(ErrorHandle::argument("unknown TLS backend")),
        };
        let enabled = match backend {
            TlsBackend::Rustls => rumqttc_library_capabilities() & CAP_RUSTLS != 0,
            TlsBackend::Native => rumqttc_library_capabilities() & CAP_NATIVE_TLS != 0,
        };
        let capabilities = if enabled {
            backend.capabilities()
        } else {
            rumqttc_wrapper_core::TlsCapabilities::default()
        };
        unsafe {
            (*out).version_policy_mask = capabilities.version_policies;
            (*out).root_policy_mask = capabilities.root_policies;
            (*out).pin_target_mask = u32::from(capabilities.certificate_sha256_pins)
                | (u32::from(capabilities.spki_sha256_pins) << 1);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_profile_new(
    options: *const rumqttc_tls_profile_options_t,
    out: *mut *mut rumqttc_tls_profile,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() || options.is_null() {
            return Err(ErrorHandle::argument(
                "TLS profile options or output is NULL",
            ));
        }
        let config = unsafe { parse_profile_options(options) }?;
        config
            .validate()
            .map_err(|error| ErrorHandle::from_core(&error, None))?;
        unsafe { *out = Box::into_raw(Box::new(rumqttc_tls_profile { config })) };
        Ok(())
    })
}

pub(super) unsafe fn parse_profile_options(
    options: *const rumqttc_tls_profile_options_t,
) -> Result<TlsConfig, ErrorHandle> {
    if options.is_null() {
        return Err(ErrorHandle::argument("TLS profile options is NULL"));
    }
    if unsafe { ptr::addr_of!((*options).struct_size).read() }
        < struct_size::<rumqttc_tls_profile_options_t>()
    {
        return Err(ErrorHandle::argument(
            "TLS profile options size is too small",
        ));
    }
    let options = unsafe { &*options };
    if options.reserved != [0; 2] {
        return Err(ErrorHandle::argument("invalid TLS profile reserved fields"));
    }
    let mut config = if options.tls.is_null() {
        TlsConfig::default()
    } else {
        unsafe { parse_tls_options(options.tls) }?
    };
    if match config.backend {
        TlsBackend::Rustls => rumqttc_library_capabilities() & CAP_RUSTLS == 0,
        TlsBackend::Native => rumqttc_library_capabilities() & CAP_NATIVE_TLS == 0,
    } {
        return Err(ErrorHandle::plain(
            crate::error::CONFIG_ERROR,
            1,
            "selected TLS backend is unavailable in this library",
        ));
    }
    config.version_policy = match options.version_policy {
        0 => TlsVersionPolicy::Default,
        1 => TlsVersionPolicy::Tls12Only,
        2 => TlsVersionPolicy::Tls13Only,
        3 => TlsVersionPolicy::Tls12OrTls13,
        _ => return Err(ErrorHandle::argument("unknown TLS version policy")),
    };
    if options.pin_count > MAX_TLS_PINS || (options.pin_count != 0 && options.pins.is_null()) {
        return Err(ErrorHandle::argument("invalid TLS pin array"));
    }
    let pins = if options.pin_count == 0 {
        &[][..]
    } else {
        unsafe { slice::from_raw_parts(options.pins, options.pin_count) }
    };
    for pin in pins {
        if pin.struct_size < struct_size::<rumqttc_tls_pin_t>() || pin.reserved != [0; 2] {
            return Err(ErrorHandle::argument("invalid TLS pin record"));
        }
        let target = match pin.target {
            0 => TlsPinTarget::LeafCertificate,
            1 => TlsPinTarget::LeafSpki,
            _ => return Err(ErrorHandle::argument("unknown TLS pin target")),
        };
        config.pins.push(TlsPin {
            target,
            sha256: pin.sha256,
        });
    }
    Ok(config)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_tls_profile_destroy(profile: *mut rumqttc_tls_profile) {
    unsafe { destroy_box(profile) };
}

unsafe fn profile_config(profile: *const rumqttc_tls_profile) -> Result<TlsConfig, ErrorHandle> {
    if profile.is_null() {
        return Err(ErrorHandle::argument("TLS profile is NULL"));
    }
    Ok(unsafe { &*profile }.config.clone())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_transport_tls_with_profile(
    config: *mut rumqttc_config,
    profile: *const rumqttc_tls_profile,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        set_transport_tls(config, unsafe { profile_config(profile) }?);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_transport_wss_with_profile(
    config: *mut rumqttc_config,
    url: rumqttc_string_view_t,
    profile: *const rumqttc_tls_profile,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        if !cfg!(feature = "websocket") {
            return Err(ErrorHandle::plain(
                crate::error::CONFIG_ERROR,
                1,
                "WebSocket feature is disabled",
            ));
        }
        let url = unsafe { string_from_view(url) }?;
        set_transport_wss(config, url, unsafe { profile_config(profile) }?);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_proxy_with_tls_profile(
    config: *mut rumqttc_config,
    options: *const rumqttc_proxy_options_t,
    profile: *const rumqttc_tls_profile,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let tls = unsafe { profile_config(profile) }?;
        let proxy = unsafe { parse_proxy_options_with_tls(options, Some(tls)) }?;
        config.common.proxy = Some(proxy);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_redirect_policy_with_tls_profile(
    config: *mut rumqttc_config,
    max_attempts: u32,
    transport: u32,
    profile: *const rumqttc_tls_profile,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            return Err(ErrorHandle::argument("redirect policy requires MQTT 5"));
        };
        if max_attempts == 0 {
            return Err(ErrorHandle::argument("redirect attempts must be nonzero"));
        }
        let tls = unsafe { profile_config(profile) }?;
        let transport = match transport {
            1 => TransportConfig::Tls(tls),
            3 if cfg!(feature = "websocket") => TransportConfig::Wss(tls),
            _ => {
                return Err(ErrorHandle::argument(
                    "TLS profile requires an available TLS or WSS redirect transport",
                ));
            }
        };
        v5.redirect_policy = rumqttc_wrapper_core::RedirectPolicy::Follow {
            max_attempts: max_attempts as usize,
            transport,
        };
        Ok(())
    })
}
