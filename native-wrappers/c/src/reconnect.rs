use std::ptr;
use std::time::Duration;

use rumqttc_wrapper_core::{
    Completion, ReconnectConfig, ReconnectDiagnostics, ReconnectJitter, ReconnectPolicy,
    RetryBudget,
};

use super::{
    boundary, client_ref, completion_ref, config_update, core_error, error_ref, observe_completion,
    rumqttc_client, rumqttc_completion, rumqttc_config, rumqttc_error, struct_size, write_optional,
};
use crate::error::ErrorHandle;

#[repr(C)]
pub struct rumqttc_reconnect_options_t {
    pub struct_size: u32,
    pub mode: u32,
    pub jitter: u32,
    pub budget_kind: u32,
    pub multiplier: u32,
    pub reserved: u32,
    pub initial_delay_ms: u64,
    pub maximum_delay_ms: u64,
    pub retry_limit: u64,
    pub stability_interval_ms: u64,
}

#[repr(C)]
pub struct rumqttc_reconnect_diagnostics_t {
    pub struct_size: u32,
    pub mode: u32,
    pub phase: u32,
    pub budget_kind: u32,
    pub cycles_started: u64,
    pub retries_since_reset: u64,
    pub retry_limit: u64,
    pub reset_count: u64,
    pub delay_present: u8,
    pub stability_present: u8,
    pub last_failure_present: u8,
    pub reserved: u8,
    pub stop_reason: u32,
    pub remaining_delay_at_capture_ms: u64,
    pub remaining_stability_at_capture_ms: u64,
    pub snapshot_age_ms: u64,
}

unsafe fn parse(
    options: *const rumqttc_reconnect_options_t,
) -> Result<ReconnectPolicy, ErrorHandle> {
    if options.is_null() {
        return Err(ErrorHandle::argument("reconnect options are NULL"));
    }
    if unsafe { (*options).struct_size } < struct_size::<rumqttc_reconnect_options_t>() {
        return Err(ErrorHandle::argument("reconnect options are too small"));
    }
    let options = unsafe { &*options };
    if options.reserved != 0 || options.mode > 1 {
        return Err(ErrorHandle::argument("invalid reconnect options"));
    }
    let jitter = match options.jitter {
        0 => ReconnectJitter::None,
        1 => ReconnectJitter::Full,
        _ => return Err(ErrorHandle::argument("unknown reconnect jitter")),
    };
    let budget = match options.budget_kind {
        0 => RetryBudget::Limited(options.retry_limit),
        1 if options.retry_limit == 0 => RetryBudget::Unlimited,
        _ => return Err(ErrorHandle::argument("invalid reconnect budget")),
    };
    let config = ReconnectConfig {
        initial_delay: Duration::from_millis(options.initial_delay_ms),
        maximum_delay: Duration::from_millis(options.maximum_delay_ms),
        multiplier: options.multiplier,
        jitter,
        budget,
        stability_interval: Duration::from_millis(options.stability_interval_ms),
    };
    config
        .validate()
        .map_err(|error| core_error(&error, None))?;
    Ok(if options.mode == 0 {
        ReconnectPolicy::Legacy
    } else {
        ReconnectPolicy::Classified(config)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_reconnect_policy(
    config: *mut rumqttc_config,
    options: *const rumqttc_reconnect_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.reconnect = unsafe { parse(options) }?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_reconnect_policy(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.reconnect = ReconnectPolicy::Legacy;
        Ok(())
    })
}

fn milliseconds(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

unsafe fn reset(out: *mut rumqttc_reconnect_diagnostics_t) -> Result<(), ErrorHandle> {
    if out.is_null() {
        return Err(ErrorHandle::argument(
            "reconnect diagnostics output is NULL",
        ));
    }
    let size = unsafe { (*out).struct_size };
    if size < struct_size::<rumqttc_reconnect_diagnostics_t>() {
        return Err(ErrorHandle::argument(
            "reconnect diagnostics output is too small",
        ));
    }
    // Preserve the caller's size and leave any future extension bytes untouched.
    unsafe {
        *out = rumqttc_reconnect_diagnostics_t {
            struct_size: size,
            mode: 0,
            phase: 0,
            budget_kind: 0,
            cycles_started: 0,
            retries_since_reset: 0,
            retry_limit: 0,
            reset_count: 0,
            delay_present: 0,
            stability_present: 0,
            last_failure_present: 0,
            reserved: 0,
            stop_reason: 0,
            remaining_delay_at_capture_ms: 0,
            remaining_stability_at_capture_ms: 0,
            snapshot_age_ms: 0,
        };
    }
    Ok(())
}

unsafe fn fill(out: *mut rumqttc_reconnect_diagnostics_t, value: &ReconnectDiagnostics) {
    let out = unsafe { &mut *out };
    out.mode = u32::from(value.classified);
    out.phase = value.phase as u32;
    (out.budget_kind, out.retry_limit) = match value.budget {
        RetryBudget::Limited(limit) => (0, limit),
        RetryBudget::Unlimited => (1, 0),
    };
    out.cycles_started = value.cycles_started;
    out.retries_since_reset = value.retries_since_reset;
    out.reset_count = value.reset_count;
    out.delay_present = u8::from(value.remaining_delay.is_some());
    out.stability_present = u8::from(value.remaining_stability.is_some());
    out.last_failure_present = u8::from(value.last_failure.is_some());
    out.stop_reason = value.stop_reason as u32;
    out.remaining_delay_at_capture_ms = milliseconds(value.remaining_delay.unwrap_or_default());
    out.remaining_stability_at_capture_ms =
        milliseconds(value.remaining_stability.unwrap_or_default());
    out.snapshot_age_ms = milliseconds(value.captured_at.elapsed());
}

fn completion_snapshot(
    completion: *const rumqttc_completion,
) -> Result<Box<ReconnectDiagnostics>, ErrorHandle> {
    let completion = unsafe { completion_ref(completion) }?;
    let value = observe_completion(completion, None)?
        .ok_or_else(|| ErrorHandle::would_block("completion is not ready"))?;
    let Completion::Diagnostics(value) = value else {
        return Err(ErrorHandle::state("completion is not diagnostics"));
    };
    value
        .reconnect
        .ok_or_else(|| ErrorHandle::state("reconnect diagnostics are unavailable"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_reconnect_diagnostics(
    client: *mut rumqttc_client,
    out: *mut rumqttc_reconnect_diagnostics_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, client, || {
        unsafe { reset(out) }?;
        let value = unsafe { client_ref(client) }?
            .handle
            .reconnect_diagnostics();
        unsafe {
            fill(out, &value);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_reconnect_diagnostics(
    completion: *const rumqttc_completion,
    out: *mut rumqttc_reconnect_diagnostics_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { reset(out) }?;
        let value = completion_snapshot(completion)?;
        unsafe {
            fill(out, &value);
        }
        Ok(())
    })
}

fn write_failure(out: *mut *mut rumqttc_error, failure: Option<&rumqttc_wrapper_core::Error>) {
    if let Some(failure) = failure {
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_error {
                inner: core_error(failure, None),
            }));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_reconnect_last_error(
    client: *mut rumqttc_client,
    out: *mut *mut rumqttc_error,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, client, || {
        if out.is_null() || out == error_out {
            return Err(ErrorHandle::argument("invalid last error output"));
        }
        let value = unsafe { client_ref(client) }?
            .handle
            .reconnect_diagnostics();
        write_failure(out, value.last_failure.as_ref());
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_reconnect_last_error(
    completion: *const rumqttc_completion,
    out: *mut *mut rumqttc_error,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() || out == error_out {
            return Err(ErrorHandle::argument("invalid last error output"));
        }
        let value = completion_snapshot(completion)?;
        write_failure(out, value.last_failure.as_ref());
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_reconnect_exhaustion(
    error: *const rumqttc_error,
    present_out: *mut u8,
    cycles_started_out: *mut u64,
    retries_since_reset_out: *mut u64,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(cycles_started_out, 0);
        write_optional(retries_since_reset_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null()
            && cycles_started_out.is_null()
            && retries_since_reset_out.is_null()
        {
            return Err(ErrorHandle::argument("exhaustion outputs are NULL"));
        }
        let error = unsafe { error_ref(error) }?;
        if let Some(value) = &error.exhaustion {
            unsafe {
                write_optional(present_out, 1);
                write_optional(cycles_started_out, value.cycles_started);
                write_optional(retries_since_reset_out, value.retries_since_reset);
            }
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_reconnect_last_error(
    error: *const rumqttc_error,
    out: *mut *mut rumqttc_error,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() || out == error_out {
            return Err(ErrorHandle::argument("invalid last error output"));
        }
        let error = unsafe { error_ref(error) }?;
        write_failure(
            out,
            error
                .exhaustion
                .as_ref()
                .and_then(|value| value.last_failure.as_ref()),
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> rumqttc_reconnect_options_t {
        rumqttc_reconnect_options_t {
            struct_size: struct_size::<rumqttc_reconnect_options_t>(),
            mode: 1,
            jitter: 1,
            budget_kind: 1,
            multiplier: 2,
            reserved: 0,
            initial_delay_ms: 1000,
            maximum_delay_ms: 60000,
            retry_limit: 0,
            stability_interval_ms: 30000,
        }
    }

    #[test]
    fn initializer_matches_core_defaults_and_finite_zero_is_not_unlimited() {
        let mut value = options();
        assert_eq!(
            unsafe { parse(&raw const value) }.unwrap(),
            ReconnectPolicy::Classified(ReconnectConfig::default())
        );
        value.budget_kind = 0;
        let ReconnectPolicy::Classified(policy) = unsafe { parse(&raw const value) }.unwrap()
        else {
            panic!("expected classified policy");
        };
        assert_eq!(policy.budget, RetryBudget::Limited(0));
    }

    #[test]
    fn invalid_setter_is_atomic_and_clear_restores_legacy() {
        let mut config = super::super::rumqttc_config {
            inner: crate::config::ConfigHandle::new(1).unwrap(),
        };
        let mut value = options();
        assert_eq!(
            unsafe {
                rumqttc_config_set_reconnect_policy(
                    &raw mut config,
                    &raw const value,
                    ptr::null_mut(),
                )
            },
            crate::error::OK
        );
        let before = config.inner.clone_config().unwrap();
        for field in 0..7 {
            value = options();
            match field {
                0 => value.struct_size = 0,
                1 => value.mode = 2,
                2 => value.jitter = 2,
                3 => value.budget_kind = 2,
                4 => value.retry_limit = 1,
                5 => value.multiplier = 0,
                _ => value.maximum_delay_ms = 1,
            }
            assert_ne!(
                unsafe {
                    rumqttc_config_set_reconnect_policy(
                        &raw mut config,
                        &raw const value,
                        ptr::null_mut(),
                    )
                },
                crate::error::OK
            );
            assert_eq!(config.inner.clone_config().unwrap(), before);
        }
        value = options();
        value.reserved = 1;
        assert!(unsafe { parse(&raw const value) }.is_err());
        assert!(unsafe { parse(ptr::null()) }.is_err());
        assert_eq!(
            unsafe { rumqttc_config_clear_reconnect_policy(&raw mut config, ptr::null_mut()) },
            crate::error::OK
        );
        assert_eq!(
            config.inner.clone_config().unwrap().common.reconnect,
            ReconnectPolicy::Legacy
        );
    }

    #[test]
    fn failed_observations_initialize_all_known_outputs() {
        let mut out = rumqttc_reconnect_diagnostics_t {
            struct_size: struct_size::<rumqttc_reconnect_diagnostics_t>(),
            mode: 99,
            phase: 99,
            budget_kind: 99,
            cycles_started: 99,
            retries_since_reset: 99,
            retry_limit: 99,
            reset_count: 99,
            delay_present: 99,
            stability_present: 99,
            last_failure_present: 99,
            reserved: 99,
            stop_reason: 99,
            remaining_delay_at_capture_ms: 99,
            remaining_stability_at_capture_ms: 99,
            snapshot_age_ms: 99,
        };
        assert_eq!(
            unsafe {
                rumqttc_client_reconnect_diagnostics(ptr::null_mut(), &raw mut out, ptr::null_mut())
            },
            crate::error::INVALID_ARGUMENT
        );
        assert_eq!(
            (
                out.mode,
                out.phase,
                out.cycles_started,
                out.last_failure_present
            ),
            (0, 0, 0, 0)
        );
        let mut present = 99;
        let mut cycles = 99;
        let mut retries = 99;
        assert_eq!(
            unsafe {
                rumqttc_error_reconnect_exhaustion(
                    ptr::null(),
                    &raw mut present,
                    &raw mut cycles,
                    &raw mut retries,
                )
            },
            crate::error::INVALID_ARGUMENT
        );
        assert_eq!((present, cycles, retries), (0, 0, 0));
    }
}
