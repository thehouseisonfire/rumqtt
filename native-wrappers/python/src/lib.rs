mod client;
mod command;
mod completion;
mod config;
mod error;
mod event;

use pyo3::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "panic-testing")]
pub(crate) struct InjectedAsyncPanic;

#[cfg(feature = "panic-testing")]
fn install_test_panic_hook() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !info.payload().is::<InjectedAsyncPanic>() {
                previous(info);
            }
        }));
    });
}

const MAX_BLOCKING_THREADS: usize = 2;
static CONFIGURED_BLOCKING_THREADS: AtomicUsize = AtomicUsize::new(MAX_BLOCKING_THREADS);
#[cfg(feature = "benchmark-testing")]
const BLOCKING_THREADS_ENV: &str = "RUMQTTC_TOKIO_BLOCKING_THREADS";

#[cfg(feature = "benchmark-testing")]
fn blocking_threads() -> PyResult<usize> {
    use pyo3::exceptions::PyValueError;

    let Ok(value) = std::env::var(BLOCKING_THREADS_ENV) else {
        return Ok(MAX_BLOCKING_THREADS);
    };
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value >= 2)
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "{BLOCKING_THREADS_ENV} must be an integer of at least 2"
            ))
        })
}

#[cfg(not(feature = "benchmark-testing"))]
const fn blocking_threads() -> usize {
    MAX_BLOCKING_THREADS
}

fn configure_tokio_runtime(blocking_threads: usize) -> usize {
    static CONFIGURE: std::sync::Once = std::sync::Once::new();
    CONFIGURE.call_once(|| {
        CONFIGURED_BLOCKING_THREADS.store(blocking_threads, Ordering::Release);
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all().max_blocking_threads(blocking_threads);
        pyo3_async_runtimes::tokio::init(builder);
    });
    CONFIGURED_BLOCKING_THREADS.load(Ordering::Acquire)
}

pub(crate) fn native_blocking_capacity() -> usize {
    CONFIGURED_BLOCKING_THREADS.load(Ordering::Acquire) - 1
}

#[pymodule]
#[pyo3(name = "_native")]
fn rumqttc_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    #[cfg(feature = "panic-testing")]
    install_test_panic_hook();
    #[cfg(feature = "benchmark-testing")]
    let configured_blocking_threads = configure_tokio_runtime(blocking_threads()?);
    #[cfg(not(feature = "benchmark-testing"))]
    configure_tokio_runtime(blocking_threads());

    module.add_class::<client::NativeMqttClient>()?;
    module.add_class::<completion::NativeCompletion>()?;
    #[cfg(feature = "benchmark-testing")]
    module.add("_TOKIO_BLOCKING_THREADS", configured_blocking_threads)?;
    Ok(())
}
