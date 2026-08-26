mod client;
mod command;
mod completion;
mod config;
mod error;
mod event;

use pyo3::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

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
const fn blocking_threads() -> PyResult<usize> {
    Ok(MAX_BLOCKING_THREADS)
}

fn configure_tokio_runtime() -> PyResult<usize> {
    static CONFIGURE: std::sync::Once = std::sync::Once::new();
    let blocking_threads = blocking_threads()?;

    CONFIGURE.call_once(|| {
        CONFIGURED_BLOCKING_THREADS.store(blocking_threads, Ordering::Release);
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.enable_all().max_blocking_threads(blocking_threads);
        pyo3_async_runtimes::tokio::init(builder);
    });
    Ok(CONFIGURED_BLOCKING_THREADS.load(Ordering::Acquire))
}

pub(crate) fn native_blocking_capacity() -> usize {
    CONFIGURED_BLOCKING_THREADS.load(Ordering::Acquire) - 1
}

#[pymodule]
#[pyo3(name = "_native")]
fn rumqttc_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let _blocking_threads = configure_tokio_runtime()?;

    module.add_class::<client::NativeMqttClient>()?;
    module.add_class::<completion::NativeCompletion>()?;
    #[cfg(feature = "benchmark-testing")]
    module.add("_TOKIO_BLOCKING_THREADS", _blocking_threads)?;
    Ok(())
}
