mod client;
mod command;
mod completion;
mod config;
mod error;
mod event;

use pyo3::prelude::*;

#[pymodule]
#[pyo3(name = "_native")]
fn rumqttc_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<client::NativeMqttClient>()?;
    module.add_class::<completion::NativeCompletion>()?;
    Ok(())
}
